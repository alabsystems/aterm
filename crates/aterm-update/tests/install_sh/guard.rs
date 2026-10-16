// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The whole script, run as a person runs it: the whole-file execution guard
//! (a truncated `curl … | bash` runs nothing), install-mode `--dry-run`'s
//! zero-mutation plan, the toolset stand-down, and `--uninstall`'s ownership.
//! Every run has a scratch HOME and TMPDIR and a PATH of the stubs it names;
//! the network is a logger or a stub that serves one release.

use std::path::Path;

use crate::support::{
    Home, Lib, Run, SYSTEM_PATH, Scratch, bash, hermetic, install_sh, installer, q, read, run, s,
    symlink, tree, write,
};

/// curl and gh as loggers: any invocation is recorded, never networked.
const LOGGER: &str = "#!/bin/sh\necho \"$(basename \"$0\") $*\" >>\"${SHIM_LOG:?}\"\nexit 1\n";

/// `uname` answering Linux, so a run that breached the guard cannot reach the
/// machine's own /Applications/aterm.app (the macOS lane links it, then runs
/// its toolset over the network); it still asks the logged curl for the head.
const LINUX_UNAME: &str = "#!/bin/sh\ncase \"$1\" in -m) echo x86_64 ;; *) echo Linux ;; esac\n";

/// The cut points that matter: each line that starts a top-level construct
/// and the line before it (bash executes complete top-level statements as
/// they arrive, so every truncation behaves like one of these), plus the
/// three classic shapes — everything before `main`, a cut deep inside it, and
/// a cut that loses only the final call.
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

/// What a truncated run did: its status and output, and whatever it wrote or
/// asked of the network.
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
    // The prefix arrives on bash's stdin, as `curl … | bash` delivers it.
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.arg("-c")
        .arg(format!("exec {} <{}", q(s(&bash())), q(s(&file))))
        .env_clear();
    let path = format!("{}:{SYSTEM_PATH}", s(shim));
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

/// The cuts of `text` (from `select`) that executed something: wrote into
/// HOME or TMPDIR, reached the network, or exited 0 with output (a prefix of
/// the run).
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
            // Inert: a parse error (bash's status 2), or a silent exit 0.
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

/// A `curl … | bash` cut short anywhere runs nothing: a cut inside `main`'s
/// body is a parse error, and a cut that keeps every definition but loses the
/// final `main "$@"` defines everything and calls nothing.
#[test]
fn a_truncated_pipe_runs_nothing() {
    let scratch = Scratch::new();
    let text = std::fs::read_to_string(install_sh()).expect("install.sh");
    let breaches = guard_breaches(&scratch, &text, |all| all);
    assert!(breaches.is_empty(), "{}", breaches.join("\n"));
}

/// Negative control: with a statement after `main "$@"`, the guard's call is
/// no longer the last line, and a cut between them runs the installer.
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

/// The release download host as one published release, `v9.9.9` of
/// `$STUB_SLUG`: the latest pointer, the manifest's presence, the manifest's
/// and the zip's sizes, and the manifest's bytes. Anything else — any other
/// URL or request shape, any API host — fails the run.
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

struct DryRun {
    run: Run,
    requests: Vec<String>,
}

fn dry_run(scratch: &Scratch, home: &Home, args: &[&str]) -> DryRun {
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
    let path = format!("{}:{SYSTEM_PATH}", s(&web));
    let run = installer(
        &install_sh(),
        home,
        &[
            ("PATH", &path),
            ("ATERM_REPO_SLUG", "example/aterm"),
            ("STUB_SLUG", "example/aterm"),
            ("STUB_LOG", s(&log)),
            ("STUB_MANIFEST", s(&manifest)),
        ],
        args,
    );
    DryRun {
        run,
        requests: read(&log).lines().map(str::to_string).collect(),
    }
}

/// Install-mode `--dry-run` prints the whole plan — the elected release, the
/// exact asset and size, every destination — reads the latest pointer exactly
/// once by a redirect-refusing HEAD, asks nothing of any other host or of gh,
/// and leaves HOME and TMPDIR untouched. A bundle at the destination that is
/// not aterm's skips the app half, left in place.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "the stubbed dry run models the macOS app lane"
)]
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
        !plan.contains("marker-fenced block"),
        "no toolset hook, no rc edit: {plan}"
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

    // With the toolset hook already laid, the plan discloses the rc edit it
    // would make — and still writes nothing.
    let hooked = scratch.home("dry-hook");
    write(&hooked.home.join(".aterm/shell.d/00-atpkg.zsh"), "");
    let before = tree(&hooked.home);
    let dry = dry_run(&scratch, &hooked, &["--dry-run", "--no-toolchain"]);
    let plan = dry.run.all();
    assert_eq!(dry.run.code(), 0, "{plan}");
    assert!(
        plan.contains(&format!(
            "marker-fenced block to {}/.zshrc",
            s(&hooked.home)
        )) && plan.contains("--no-path"),
        "{plan}"
    );
    assert_eq!(tree(&hooked.home), before);
    assert_eq!(tree(&hooked.tmp), Vec::<String>::new());
}

/// The PATH block's marker pair and the store's argv0 aliases, as the
/// installer defines them.
fn installer_constants(scratch: &Scratch) -> (String, String, Vec<String>) {
    let mut lib = Lib::new();
    let call = lib.call(
        ":",
        &[
            "ATERM_PATH_BLOCK_START_MARKER",
            "ATERM_PATH_BLOCK_END_MARKER",
            "STORE_ALIASES",
        ],
    );
    let recs = lib.run(scratch);
    let rec = &recs[call];
    (
        rec.var("ATERM_PATH_BLOCK_START_MARKER").to_string(),
        rec.var("ATERM_PATH_BLOCK_END_MARKER").to_string(),
        rec.var("STORE_ALIASES")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
    )
}

/// `--uninstall --dry-run` decides about what it owns — a planted managed
/// PATH block — and leaves the profile byte for byte.
#[test]
fn an_uninstall_dry_run_plans_and_changes_nothing() {
    let scratch = Scratch::new();
    let (start, end, _) = installer_constants(&scratch);
    let home = scratch.home("u");
    let rc = home.home.join(".zshrc");
    let profile = format!("{start}\nexport PATH=\"$HOME/.local/bin:$PATH\"\n{end}\n");
    write(&rc, &profile);
    let run = installer(&install_sh(), &home, &[], &["--uninstall", "--dry-run"]);
    assert_eq!(run.code(), 0, "{}", run.all());
    assert!(
        run.stdout
            .contains(&format!("would remove PATH block: {}", s(&rc))),
        "{}",
        run.all()
    );
    assert!(run.stdout.contains("1 item(s)"), "{}", run.all());
    assert_eq!(read(&rc), profile);
    assert_eq!(tree(&home.home), [".zshrc"]);
}

/// A toolchain-only run that stands aside for another atpkg run (`pkg seed`
/// exits 75 on the store lock) is not a failure: one `pkg seed`, no update
/// after it, the rerun named, exit 0 — never followed by a failure line, and
/// never naming the holder, which install.sh cannot see.
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
    // The toolset half on, driving the stub above, never an installed aterm.
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

/// An aterm bundle whose atpkg sweeps as `uninstall --all` does (records the
/// decline when none is there) and logs that it ran.
fn aterm_bundle(scratch: &Scratch, app: &str, swept: Option<&Path>) {
    write(
        &scratch.join(&format!("{app}/Contents/Info.plist")),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n<key>CFBundleIdentifier</key><string>com.aterm.aterm</string></dict></plist>\n",
    );
    if let Some(log) = swept {
        scratch.exe(
            &format!("{app}/Contents/MacOS/atpkg"),
            &format!(
                "#!/bin/sh\necho swept >>{}\np=\"$HOME/Library/Application Support/aterm/pkg\"\nmkdir -p \"$p\" && {{ [ -f \"$p/declined\" ] || echo declined >\"$p/declined\"; }}\n",
                q(s(log))
            ),
        );
    }
}

/// `--uninstall` removes from ATERM_STORE_DIR only what the store lanes place
/// there — the aterm binary, its argv0 aliases, the updater's dir, a staging
/// leftover — and keeps a bundle whose toolchain sweep failed, since the remedy
/// it prints runs that bundle's atpkg.
#[test]
fn uninstall_removes_only_what_it_placed() {
    let scratch = Scratch::new();
    let (_, _, aliases) = installer_constants(&scratch);
    assert!(!aliases.is_empty());
    let store = scratch.mkdir("own/store");
    scratch.mkdir("own/store/.aterm-update");
    scratch.exe("own/store/backup.sh", "#!/bin/sh\necho backup\n");
    scratch.stand_in("own/store/aterm", "#!/bin/sh\necho \"aterm 0.0.0\"\n");
    scratch.write("own/store/.aterm-install.Ab12Cd", "");
    for alias in &aliases {
        symlink("aterm", &store.join(alias));
    }
    // The store's `aterm` is probed under the installer's production bound; run
    // it once first, as the install that placed it did, so the system's
    // first-exec code assessment is not what the bound measures.
    run(aterm_uds::spawnfd::command(store.join("aterm")).arg("--version"));
    aterm_bundle(&scratch, "own/apps/aterm.app", None);
    let atpkg = scratch.exe(
        "own/apps/aterm.app/Contents/MacOS/atpkg",
        "#!/bin/sh\nexit 75\n",
    );
    let home = scratch.home("own");
    let run = installer(
        &install_sh(),
        &home,
        &[
            ("ATERM_INSTALL_DIR", s(&scratch.join("own/apps"))),
            ("ATERM_STORE_DIR", s(&store)),
        ],
        &["--uninstall"],
    );
    let left: Vec<String> = std::fs::read_dir(&store)
        .expect("the store dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["backup.sh"], "{}", run.all());
    assert!(
        atpkg.exists(),
        "the bundle whose sweep failed is kept: {}",
        run.all()
    );
    assert_eq!(run.code(), 1, "{}", run.all());
}

/// A scoped `--uninstall` (ATERM_INSTALL_DIR) removes the bundle it scopes and
/// leaves what another aterm.app on the account uses — the toolchain store,
/// the PATH links, the PATH block, the man pages. Once the scoped copy is the
/// only aterm, its sweep runs, and the decline that sweep records goes with it.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "bundles are read with `defaults`, which only macOS has"
)]
fn a_scoped_uninstall_leaves_what_another_aterm_uses() {
    let scratch = Scratch::new();
    let (start, end, _) = installer_constants(&scratch);
    let scoped = scratch.home("scoped");
    let home = scoped.home.clone();
    let swept = scratch.join("scoped/swept.log");
    let man = scratch.write("scoped/home/.local/share/man/man1/aterm.1", "");
    let rc = scratch.write(
        "scoped/home/.zshrc",
        format!("{start}\nexport PATH=\"$PATH:$HOME/.local/bin\"\n{end}\n"),
    );
    aterm_bundle(&scratch, "scoped/home/Applications/aterm.app", Some(&swept));
    let links = scratch.mkdir("scoped/home/.local/bin");
    for tool in ["aterm", "atpkg"] {
        symlink(
            home.join(format!("Applications/aterm.app/Contents/MacOS/{tool}")),
            &links.join(tool),
        );
    }
    aterm_bundle(&scratch, "scoped/home/aterm.app", Some(&swept));
    let env = [("ATERM_INSTALL_DIR", s(&home))];
    let run = installer(&install_sh(), &scoped, &env, &["--uninstall"]);
    assert_eq!(run.code(), 0, "{}", run.all());
    assert!(
        !swept.exists(),
        "the other aterm's store was not swept: {}",
        run.all()
    );
    assert!(links.join("aterm").is_symlink() && links.join("atpkg").is_symlink());
    assert!(man.exists());
    assert_eq!(read(&rc).lines().filter(|l| *l == start).count(), 1);
    assert!(!home.join("aterm.app").exists());

    std::fs::remove_dir_all(home.join("Applications")).expect("rm");
    aterm_bundle(&scratch, "scoped/home/aterm.app", Some(&swept));
    let run = installer(&install_sh(), &scoped, &env, &["--uninstall"]);
    assert_eq!(run.code(), 0, "{}", run.all());
    assert!(swept.exists(), "the only aterm's sweep ran");
    assert!(
        !home
            .join("Library/Application Support/aterm/pkg/declined")
            .exists(),
        "the decline its sweep recorded goes with it"
    );
    assert!(!links.join("aterm").is_symlink() && !links.join("atpkg").is_symlink());
    assert!(!home.join("aterm.app").exists());
}

/// A store that overlaps a PATH directory is refused before anything is
/// fetched: ATERM_BIN_DIR itself (through a dir still to be created, a `..`
/// and a symlink), and a store around ~/.local/bin.
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
    let path = format!("{}:{SYSTEM_PATH}", s(&shim));
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
