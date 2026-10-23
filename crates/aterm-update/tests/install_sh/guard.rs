// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-file execution guard, `--dry-run`, toolset stand-down, and `--uninstall` tests for `tools/install.sh`.

use std::path::Path;

#[cfg(target_os = "macos")]
use crate::support::Home;
use crate::support::{
    Run, Scratch, bash, hermetic, install_sh, installer, q, read, run, s, symlink, system_path,
    tree, write,
};

const LOGGER: &str = "#!/bin/sh\necho \"$(basename \"$0\") $*\" >>\"${SHIM_LOG:?}\"\nexit 1\n";

const LINUX_UNAME: &str = "#!/bin/sh\ncase \"$1\" in -m) echo x86_64 ;; *) echo Linux ;; esac\n";

/// Returns truncation line numbers at top-level construct boundaries plus pre-`main`, mid-`main`, and final-line cuts.
fn cuts(lines: &[&str]) -> Vec<usize> {
    let total = lines.len();
    let main_open = lines
        .iter()
        .position(|l| *l == "main() {")
        .expect("install.sh defines main")
        + 1;
    let mut cuts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            l.chars()
                .next()
                .is_some_and(|c| !c.is_whitespace() && c != '#')
        })
        .flat_map(|(at, _)| [at, at + 1])
        .chain([main_open - 1, (main_open + total) / 2, total - 1])
        .filter(|cut| (1..total).contains(cut))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

struct Truncated {
    cut: usize,
    run: Run,
    home: Vec<String>,
    tmp: Vec<String>,
    calls: String,
}

fn truncated(scratch: &Scratch, text: &str, cut: usize, shim: &Path) -> Truncated {
    let home = scratch.home(&format!("cut/{cut}"));
    let log = scratch.join(&format!("cut/{cut}/calls.log"));
    let prefix: String = text.split_inclusive('\n').take(cut).collect();
    let file = scratch.write(&format!("cut/{cut}/prefix.sh"), prefix);
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.arg("-c")
        .arg(format!("exec {} <{}", q(s(&bash())), q(s(&file))))
        .env_clear();
    let path = format!("{}:{}", s(shim), system_path());
    for (key, value) in hermetic(&home, &[("PATH", &path), ("SHIM_LOG", s(&log))]) {
        cmd.env(key, value);
    }
    Truncated {
        cut,
        run: run(&mut cmd),
        home: tree(&home.home),
        tmp: tree(&home.tmp),
        calls: read(&log),
    }
}

fn guard_breaches(
    scratch: &Scratch,
    text: &str,
    select: fn(Vec<usize>) -> Vec<usize>,
) -> Vec<String> {
    let shim = scratch.mkdir("shim");
    scratch.stand_in("shim/curl", LOGGER);
    scratch.stand_in("shim/gh", LOGGER);
    scratch.stand_in("shim/uname", LINUX_UNAME);
    let lines: Vec<&str> = text.lines().collect();
    let cuts = select(cuts(&lines));
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(cut) = cuts.get(at) else { break };
                    let t = truncated(scratch, text, *cut, &shim);
                    results.lock().expect("results").push(t);
                }
            });
        }
    });
    let mut results = results.into_inner().expect("results");
    assert_eq!(results.len(), cuts.len(), "every cut ran");
    results.sort_by_key(|t| t.cut);
    results
        .into_iter()
        .filter_map(|t| {
            let inert = match t.run.code() {
                0 => t.run.all().is_empty(),
                code => code == 2,
            };
            (!(t.home.is_empty() && t.tmp.is_empty() && t.calls.is_empty() && inert))
                .then(|| {
                    format!(
                        "cut at line {} ({:?}): exit {:?}, HOME {:?}, TMPDIR {:?}, calls {:?}, output {:?}",
                        t.cut,
                        lines[t.cut - 1],
                        t.run.status,
                        t.home,
                        t.tmp,
                        t.calls,
                        t.run.all().lines().next()
                    )
                })
        })
        .collect()
}

#[test]
fn a_truncated_pipe_runs_nothing() {
    let scratch = Scratch::new();
    let text = std::fs::read_to_string(install_sh()).expect("install.sh");
    let breaches = guard_breaches(&scratch, &text, |all| all);
    assert!(breaches.is_empty(), "{}", breaches.join("\n"));
}

#[test]
fn the_guard_property_catches_work_after_the_final_call() {
    let scratch = Scratch::new();
    let text = std::fs::read_to_string(install_sh()).expect("install.sh");
    let unguarded = format!("{text}: the guard is no longer last\n");
    let breaches = guard_breaches(&scratch, &unguarded, |all| all[all.len() - 3..].to_vec());
    assert!(
        breaches.iter().any(|b| b.contains("main \\\"$@\\\"")),
        "{breaches:?}"
    );
}

#[cfg(target_os = "macos")]
const WEB_STUB: &str = r#"#!/bin/sh
head=0 follow=0 fmt="" url=""
while [ $# -gt 0 ]; do
	case "$1" in
	-I) head=1 ;;
	-fsSIL) head=1; follow=1 ;;
	-fsSL) follow=1 ;;
	-w) fmt="$2"; shift ;;
	--proto | --proto-redir | --connect-timeout | --max-time | --max-filesize | --retry | -o) shift ;;
	-*) ;;
	*) url="$1" ;;
	esac
	shift
done
echo "$head$follow $url" >>"${STUB_LOG:?}"
BASE="https://github.com/${STUB_SLUG:?}/releases"
case "$head$follow $url" in
"10 $BASE/latest/download/aterm-appcast.toml")
	printf '%s' "$fmt" | sed "s#%{http_code}#302#; s#%{redirect_url}#$BASE/download/v9.9.9/aterm-appcast.toml#" ;;
"10 $BASE/download/v9.9.9/aterm-appcast.toml")
	printf '%s' "$fmt" | sed "s#%{http_code}#302#; s#%{redirect_url}#https://release-assets.githubusercontent.com/m#" ;;
"11 $BASE/download/v9.9.9/aterm-appcast.toml")
	printf 'HTTP/2 302\r\ncontent-length: 0\r\n\r\nHTTP/2 200\r\ncontent-length: %s\r\n\r\n' "$(wc -c <"${STUB_MANIFEST:?}" | tr -d ' ')" ;;
"11 $BASE/download/v9.9.9/aterm-9.9.9-mac.zip")
	printf 'HTTP/2 302\r\ncontent-length: 0\r\n\r\nHTTP/2 200\r\ncontent-length: 123456789\r\n\r\n' ;;
"01 $BASE/download/v9.9.9/aterm-appcast.toml") cat "$STUB_MANIFEST" ;;
*) echo "curl stub: unexpected request: $head$follow $url" >&2; exit 64 ;;
esac
"#;

const NO_GH: &str = "#!/bin/sh\necho \"gh stub: the installer ran gh $*\" >&2\nexit 64\n";

#[cfg(target_os = "macos")]
struct DryRun {
    run: Run,
    requests: Vec<String>,
}

#[cfg(target_os = "macos")]
fn dry_run(scratch: &Scratch, home: &Home, args: &[&str]) -> DryRun {
    dry_run_with(scratch, home, args, &[])
}

#[cfg(target_os = "macos")]
fn dry_run_with(scratch: &Scratch, home: &Home, args: &[&str], env: &[(&str, &str)]) -> DryRun {
    let web = scratch.mkdir("web");
    scratch.stand_in("web/curl", WEB_STUB);
    scratch.stand_in("web/gh", NO_GH);
    let manifest = scratch.write(
        "aterm-appcast.toml",
        format!(
            "version = \"9.9.9\"\ndmg = \"aterm-9.9.9.dmg\"\nsha256 = \"{}\"\nzip = \"aterm-9.9.9-mac.zip\"\nzip_sha256 = \"{}\"\n",
            "a".repeat(64),
            "b".repeat(64)
        ),
    );
    let log = scratch.join("requests.log");
    write(&log, "");
    let path = format!("{}:{}", s(&web), system_path());
    let mut vars = vec![
        ("PATH", path.as_str()),
        ("ATERM_REPO_SLUG", "example/aterm"),
        ("STUB_SLUG", "example/aterm"),
        ("STUB_LOG", s(&log)),
        ("STUB_MANIFEST", s(&manifest)),
    ];
    vars.extend_from_slice(env);
    let run = installer(&install_sh(), home, &vars, args);
    DryRun {
        run,
        requests: read(&log).lines().map(str::to_string).collect(),
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_dry_run_s_toolset_line_follows_the_console_owner() {
    let scratch = Scratch::new();
    // SAFETY: getuid has no preconditions and cannot fail.
    let me = unsafe { libc::getuid() };
    assert_ne!(me, 0, "the suite does not run as root");
    for (console, want, not) in [
        (
            me,
            "toolset: installs on aterm's first launch",
            "pkg update here",
        ),
        (
            me + 1,
            "toolset: aterm pkg seed + pkg update here",
            "installs on aterm's first launch",
        ),
    ] {
        let fresh = scratch.home(&format!("console-{console}"));
        let console = console.to_string();
        let dry = dry_run_with(
            &scratch,
            &fresh,
            &["--dry-run"],
            &[
                ("ATERM_INSTALL_CONSOLE_UID", console.as_str()),
                ("ATERM_NO_TOOLCHAIN", "0"),
            ],
        );
        let plan = dry.run.all();
        assert_eq!(dry.run.code(), 0, "{plan}");
        assert!(plan.contains(want), "console {console}: {plan}");
        assert!(!plan.contains(not), "console {console}: {plan}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_dry_run_over_a_home_that_cannot_be_read_still_plans() {
    let scratch = Scratch::new();
    let fresh = scratch.home("gone");
    let missing = s(&fresh.home.join("not-yet")).to_string();
    for home in [missing.as_str(), ""] {
        let dry = dry_run_with(
            &scratch,
            &fresh,
            &["--dry-run", "--no-toolchain"],
            &[("HOME", home)],
        );
        let plan = dry.run.all();
        assert_eq!(dry.run.code(), 0, "HOME={home:?}: {plan}");
        assert!(plan.contains("DRY RUN"), "HOME={home:?}: {plan}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_dry_run_prints_the_plan_and_changes_nothing() {
    let scratch = Scratch::new();
    let fresh = scratch.home("dry");
    let (home, tmp) = (&fresh.home, &fresh.tmp);
    let dry = dry_run(&scratch, &fresh, &["--dry-run", "--no-toolchain"]);
    let plan = dry.run.all();
    assert_eq!(dry.run.code(), 0, "{plan}");
    for fact in [
        "DRY RUN".to_string(),
        "nothing was installed".into(),
        "toolset: excluded (--no-toolchain".into(),
        "example/aterm v9.9.9".into(),
        "aterm-9.9.9-mac.zip — 123 MB".into(),
        format!("{}/Applications/aterm.app", s(home)),
        format!("{}/.local/bin", s(home)),
        "no pinned Team ID".into(),
    ] {
        assert!(plan.contains(&fact), "the plan names {fact:?}: {plan}");
    }
    assert!(
        plan.contains("`aterm pkg shell-setup --path --dry-run` names the file")
            && plan.contains("--no-path"),
        "the PATH edit is the installed aterm's, named with its opt-out: {plan}"
    );
    let pointer: Vec<&String> = dry
        .requests
        .iter()
        .filter(|r| r.contains("latest/download"))
        .collect();
    assert_eq!(
        pointer,
        ["10 https://github.com/example/aterm/releases/latest/download/aterm-appcast.toml"],
        "{:?}",
        dry.requests
    );
    for request in &dry.requests {
        assert!(
            request.contains(" https://github.com/example/aterm/releases/"),
            "only the release download host: {request}"
        );
    }
    for stub in ["curl stub:", "gh stub:"] {
        assert!(!plan.contains(stub), "{plan}");
    }
    assert_eq!(tree(home), Vec::<String>::new(), "HOME");
    assert_eq!(tree(tmp), Vec::<String>::new(), "TMPDIR");

    let foreign = scratch.home("dry-foreign");
    let app = foreign.home.join("Applications/aterm.app");
    write(
        &app.join("Contents/Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n<key>CFBundleIdentifier</key><string>org.example.not-aterm</string></dict></plist>\n",
    );
    let before = tree(&foreign.home);
    let dry = dry_run(&scratch, &foreign, &["--dry-run", "--no-toolchain"]);
    let plan = dry.run.all();
    assert!(
        plan.contains(&format!(
            "app: would be SKIPPED: {} — not an aterm bundle",
            s(&app)
        )),
        "{plan}"
    );
    assert_eq!(tree(&foreign.home), before);
}

#[cfg(target_os = "linux")]
const PUBLIC_NO_LINUX_STUB: &str = r#"#!/bin/sh
head=0 follow=0 fmt="" url=""
while [ $# -gt 0 ]; do
	case "$1" in
	-I) head=1 ;;
	-fsSIL) head=1; follow=1 ;;
	-fsSL) follow=1 ;;
	-w) fmt="$2"; shift ;;
	--proto | --proto-redir | --connect-timeout | --max-time | --max-filesize | --retry | -o) shift ;;
	-*) ;;
	*) url="$1" ;;
	esac
	shift
done
echo "$head$follow $url" >>"${STUB_LOG:?}"
BASE="https://github.com/alabsystems/aterm/releases"
case "$head$follow $url" in
"10 $BASE/latest/download/aterm-appcast.toml")
	printf '%s' "$fmt" | sed "s#%{http_code}#302#; s#%{redirect_url}#$BASE/download/v9.9.9/aterm-appcast.toml#" ;;
"10 $BASE/download/v9.9.9/aterm-9.9.9-linux-aarch64" | "10 $BASE/download/v9.9.9/aterm-9.9.9-linux-x86_64")
	printf '%s' "$fmt" | sed "s#%{http_code}#404#" ;;
*) echo "curl stub: unexpected request: $head$follow $url" >&2; exit 64 ;;
esac
"#;

#[cfg(target_os = "linux")]
const AARCH64_UNAME: &str = "#!/bin/sh\ncase \"$1\" in -m) echo aarch64 ;; *) echo Linux ;; esac\n";

#[cfg(target_os = "linux")]
#[test]
fn a_piped_linux_run_installs_from_the_public_channel_and_says_what_it_lacks() {
    let scratch = Scratch::new();
    let shim = scratch.mkdir("piped-linux");
    scratch.stand_in("piped-linux/curl", PUBLIC_NO_LINUX_STUB);
    scratch.stand_in("piped-linux/gh", NO_GH);
    scratch.stand_in("piped-linux/uname", AARCH64_UNAME);
    // The embedded verifier's run: a pinned aarch64 digest would fetch the static one.
    let script = crate::support::unpinned_install_sh(&scratch, "piped-linux/install.sh");
    let piped = |name: &str, args: &str| {
        let home = scratch.home(name);
        let log = scratch.join(&format!("{name}/requests.log"));
        write(&log, "");
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.arg("-c")
            .arg(format!(
                "exec {} -s -- {args} <{}",
                q(s(&bash())),
                q(s(&script))
            ))
            .env_clear();
        let path = format!("{}:{}", s(&shim), system_path());
        for (key, value) in hermetic(&home, &[("PATH", &path), ("STUB_LOG", s(&log))]) {
            cmd.env(key, value);
        }
        let ran = run(&mut cmd);
        let requests: Vec<String> = read(&log).lines().map(str::to_string).collect();
        (ran, requests)
    };
    let app_skip = "the latest release v9.9.9 publishes no Linux aarch64 executable \
                    (aterm-9.9.9-linux-aarch64) — nothing to install on this machine yet";
    let cargo_skip = "building from source needs a checkout (a piped script has none) and \
                      the Trust toolchain, which has no published build for Linux \
                      (https://github.com/alabsystems/trust builds from source); once Trust \
                      is built and linked (rustup toolchain link trust <prefix>): git clone \
                      https://github.com/alabsystems/aterm && cd aterm && tools/install.sh";
    for (name, args) in [("plan", "--dry-run"), ("real", "")] {
        let (ran, requests) = piped(name, args);
        let out = ran.all();
        assert_eq!(
            requests.first().map(String::as_str),
            Some(
                "10 https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml"
            ),
            "{name}: the public channel's latest pointer first: {requests:?}\n{out}"
        );
        for request in &requests {
            assert!(
                request.starts_with("10 https://github.com/alabsystems/aterm/releases/"),
                "{name}: only HEADs of the public channel's download host: {request}"
            );
        }
        assert!(out.contains(app_skip), "{name}: {out}");
        assert!(out.contains(cargo_skip), "{name}: {out}");
        let elsewhere = out.replace(cargo_skip, "");
        assert!(
            !elsewhere.contains("git clone"),
            "{name}: a clone offered apart from Trust: {out}"
        );
        for wrong in ["aterm.app", "--version", "tarball", "curl stub:"] {
            assert!(!out.contains(wrong), "{name}: says {wrong:?}: {out}");
        }
        if name == "plan" {
            assert_eq!(ran.code(), 0, "{out}");
        } else {
            assert_eq!(ran.code(), 1, "{out}");
            assert!(
                out.contains(
                    "SKIPPED the CLI (aterm): this run installed no released aterm (see the \
                     app line)"
                ),
                "{out}"
            );
            assert!(out.contains("install.sh: nothing was installed"), "{out}");
        }
    }
}

#[test]
fn uninstall_runs_the_installed_aterms_own_sweep() {
    let scratch = Scratch::new();
    let home = scratch.home("u");
    let store = scratch.mkdir("u/store");
    let log = scratch.join("u/calls.log");
    scratch.stand_in(
        "u/store/aterm",
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo \"aterm 0.0.0\"; exit 0; fi\n\
         echo \"$*\" >>\"${STUB_LOG:?}\"\nexit 0\n",
    );
    let apps = s(&home.home.join("Applications")).to_string();
    let run = installer(
        &install_sh(),
        &home,
        &[("ATERM_STORE_DIR", s(&store)), ("STUB_LOG", s(&log))],
        &["--uninstall", "--dry-run"],
    );
    assert_eq!(run.code(), 0, "{}", run.all());
    let calls = read(&log);
    assert!(
        calls.starts_with(
            "pkg help self-uninstall
"
        ),
        "{calls}"
    );
    assert!(
        calls.contains(&format!(
            "pkg self-uninstall --dry-run --install-dir {apps} --store-dir {}",
            s(&store)
        )),
        "{calls}"
    );
}

#[test]
fn uninstall_without_an_aterm_refuses_and_lists_what_is_there() {
    let scratch = Scratch::new();
    let home = scratch.home("none");
    let rc = home.home.join(".zshrc");
    let profile =
        "# >>> aterm ALab toolset (managed by install.sh) >>>\nx\n# <<< aterm ALab toolset <<<\n";
    write(&rc, profile);
    let ran = scratch.join("none/foreign-ran");
    let foreign = home.home.join(".local/bin/aterm");
    write(
        &foreign,
        format!("#!/bin/sh\necho \"aterm 1.0\"\ntouch {}\n", q(s(&ran))),
    );
    crate::support::chmod(&foreign, 0o755);
    let before = tree(&home.home);
    let run = installer(&install_sh(), &home, &[], &["--uninstall"]);
    assert_eq!(run.code(), 1, "{}", run.all());
    assert!(run.all().contains("nothing was removed"), "{}", run.all());
    assert!(
        run.all()
            .contains(&format!("{}: the marker-fenced aterm block", s(&rc))),
        "{}",
        run.all()
    );
    assert!(run.all().contains(s(&foreign)), "{}", run.all());
    assert!(!ran.exists(), "a foreign aterm is never run");
    assert_eq!(read(&rc), profile);
    assert_eq!(tree(&home.home), before);
}

#[test]
fn a_toolset_stand_down_is_not_a_failure() {
    let scratch = Scratch::new();
    let bin = scratch.mkdir("standdown/bin");
    let log = scratch.join("standdown/calls.log");
    scratch.stand_in(
        "standdown/bin/aterm",
        "#!/bin/sh\necho \"aterm $*\" >>\"${STUB_LOG:?}\"\nif [ \"$1 $2\" = \"pkg seed\" ]; then\n\techo \"atpkg: another atpkg process holds the store lock\" >&2\n\texit 75\nfi\nexit 0\n",
    );
    let home = scratch.home("standdown");
    let run = installer(
        &install_sh(),
        &home,
        &[
            ("ATERM_NO_TOOLCHAIN", ""),
            ("STUB_LOG", s(&log)),
            ("ATERM_BIN_DIR", s(&bin)),
        ],
        &["--no-app", "--no-cli", "--no-path"],
    );
    assert_eq!(read(&log), "aterm pkg seed\n");
    assert!(
        run.all().contains(&format!("{}/aterm pkg seed", s(&bin))),
        "the rerun is named: {}",
        run.all()
    );
    for claim in ["nothing was installed", "the app is already"] {
        assert!(!run.all().contains(claim), "{claim:?}: {}", run.all());
    }
    assert_eq!(run.code(), 0, "{}", run.all());
}

#[test]
fn a_store_overlapping_a_path_dir_is_refused_up_front() {
    let scratch = Scratch::new();
    let shim = scratch.mkdir("shim");
    scratch.stand_in("shim/curl", LOGGER);
    scratch.stand_in("shim/gh", LOGGER);
    let home = scratch.home("o");
    let pathdir = scratch.mkdir("o/pathdir");
    symlink(&pathdir, &scratch.join("o/pathlink"));
    let log = scratch.join("o/calls.log");
    let path = format!("{}:{}", s(&shim), system_path());
    for store in [
        scratch.join("o/new/../pathlink").display().to_string(),
        home.home.join(".local").display().to_string(),
    ] {
        write(&log, "");
        let run = installer(
            &install_sh(),
            &home,
            &[
                ("PATH", &path),
                ("SHIM_LOG", s(&log)),
                ("ATERM_BIN_DIR", s(&pathdir)),
                ("ATERM_STORE_DIR", &store),
            ],
            &["--dry-run", "--no-toolchain", "--no-path"],
        );
        assert_eq!(run.code(), 2, "{store}: {}", run.all());
        assert_eq!(read(&log), "", "{store}: nothing was fetched");
    }
}
