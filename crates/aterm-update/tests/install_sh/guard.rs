// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-file execution guard, `--dry-run`, toolset stand-down, and `--uninstall` tests for `tools/install.sh`.

use std::path::Path;

#[cfg(target_os = "macos")]
use crate::support::Home;
use crate::support::{
    Lib, Run, Scratch, bash, group_run, hermetic, install_sh, installer, q, read, run, s, symlink,
    system_path, tree, write,
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
    run: Run,
    home: Vec<String>,
    tmp: Vec<String>,
    calls: String,
}

/// Runs the first <cut> bytes of <text> as bash's whole input, as a `curl | bash` cut
/// there would, under a scratch home (`cut/<cut>`) with logging stand-ins on PATH. The
/// prefix goes as a file, which bash reads in blocks (a pipe it reads a byte at a time),
/// and is removed after.
fn truncated(scratch: &Scratch, text: &str, cut: usize, shim: &Path) -> Truncated {
    let home = scratch.home(&format!("cut/{cut}"));
    let log = scratch.join(&format!("cut/{cut}/calls.log"));
    let file = scratch.write(&format!("cut/{cut}/prefix.sh"), &text.as_bytes()[..cut]);
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.arg("-c")
        .arg(format!("exec {} <{}", q(s(&bash())), q(s(&file))))
        .env_clear();
    let path = format!("{}:{}", s(shim), system_path());
    for (key, value) in hermetic(&home, &[("PATH", &path), ("SHIM_LOG", s(&log))]) {
        cmd.env(key, value);
    }
    let run = run(&mut cmd);
    std::fs::remove_file(&file).expect("remove the prefix");
    Truncated {
        run,
        home: tree(&home.home),
        tmp: tree(&home.tmp),
        calls: read(&log),
    }
}

/// The stand-ins a truncated run finds first on PATH: curl and gh log their calls and fail,
/// and uname says Linux.
fn shims(scratch: &Scratch) -> std::path::PathBuf {
    let shim = scratch.mkdir("shim");
    scratch.stand_in("shim/curl", LOGGER);
    scratch.stand_in("shim/gh", LOGGER);
    scratch.stand_in("shim/uname", LINUX_UNAME);
    shim
}

/// What a cut run did that an inert one would not, named by <at>: touched its home or
/// TMPDIR, called a stand-in, or ended other than silently with 0 or by bash's syntax
/// error (a 2 alone is also `make`'s, with no Makefile to build).
fn breach(t: &Truncated, at: &str) -> Option<String> {
    let all = t.run.all();
    let inert = match t.run.code() {
        0 => all.is_empty(),
        2 => all.contains("syntax error") || all.contains("unexpected EOF"),
        _ => false,
    };
    (!(t.home.is_empty() && t.tmp.is_empty() && t.calls.is_empty() && inert)).then(|| {
        format!(
            "cut at {at}: exit {:?}, HOME {:?}, TMPDIR {:?}, calls {:?}, output {:?}",
            t.run.status,
            t.home,
            t.tmp,
            t.calls,
            t.run.all().lines().next()
        )
    })
}

/// The line cuts of <text> that <select> keeps, as [`breaches`] runs them.
fn guard_breaches(
    scratch: &Scratch,
    text: &str,
    select: fn(Vec<usize>) -> Vec<usize>,
) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let ends: Vec<usize> = text
        .split_inclusive('\n')
        .scan(0, |at, line| {
            *at += line.len();
            Some(*at)
        })
        .collect();
    let cuts = select(cuts(&lines))
        .into_iter()
        .map(|cut| (ends[cut - 1], format!("line {cut} ({:?})", lines[cut - 1])))
        .collect();
    breaches(scratch, text, cuts)
}

/// Runs each (byte offset, name) cut of <source>, eight at a time, and names each one
/// that was not inert ([`breach`]), in the order given.
fn breaches(scratch: &Scratch, text: &str, cuts: Vec<(usize, String)>) -> Vec<String> {
    let shim = shims(scratch);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((cut, name)) = cuts.get(at) else {
                        break;
                    };
                    let found = breach(&truncated(scratch, text, *cut, &shim), name);
                    results.lock().expect("results").push((at, found));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("results");
    assert_eq!(results.len(), cuts.len(), "every cut ran");
    results.sort_by_key(|(at, _)| *at);
    results.into_iter().filter_map(|(_, found)| found).collect()
}

#[test]
fn a_truncated_pipe_runs_nothing() {
    let scratch = Scratch::new();
    let text = std::fs::read_to_string(install_sh()).expect("install.sh");
    let breaches = guard_breaches(&scratch, &text, |all| all);
    assert!(breaches.is_empty(), "{}", breaches.join("\n"));
}

/// A cut inside a word runs nothing either: at every byte of each top-level line's first
/// word, and of the last line, `main "$@"; }`. The file is one group, which bash runs
/// only once its closing brace has arrived. With main alone wrapped, bash ran the cut
/// word's first letters as a command: `make` (from make_download_dir) built the
/// Makefile where the run was started, `bundle` ran bundle install, and a bare
/// `main "$@"` cut just after `main` ran a whole install with no arguments.
#[test]
fn a_cut_inside_a_word_runs_nothing() {
    let scratch = Scratch::new();
    let text = std::fs::read_to_string(install_sh()).expect("install.sh");
    let body = text.trim_end_matches('\n');
    let last = body.rfind('\n').expect("install.sh has lines") + 1;
    let mut cuts = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let word = if start == last {
            body.len() - last - 1
        } else if line.starts_with(|c: char| c.is_whitespace() || c == '#') {
            0
        } else {
            line.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(line.len())
        };
        cuts.extend((start + 1..=start + word).map(|at| (at, format!("{:?}", &text[start..at]))));
        start += line.len();
    }
    let breaches = breaches(&scratch, &text, cuts);
    assert!(breaches.is_empty(), "{}", breaches.join("\n"));
}

/// Stands for `curl | bash`: writes <prefix> into `bash -s` in a process group of
/// its own, waits for bash to create <mark>, Ctrl-Cs the group and closes the
/// pipe, as a Ctrl-C does to curl; prints how bash ended.
const FEED: &str = r#"use POSIX ();
my ($prefix, $mark, $bash) = @ARGV;
setpgrp(0, 0);
pipe(my $r, my $w) or die "pipe: $!";
my $pid = fork // die "fork: $!";
if (!$pid) {
    close $w;
    POSIX::dup2(fileno($r), 0) // die "dup2: $!";
    $SIG{INT} = "DEFAULT";
    exec $bash, "-s" or die "exec: $!";
}
close $r;
$SIG{INT} = "IGNORE";
open(my $in, "<", $prefix) or die "$prefix: $!";
my $text = do { local $/; <$in> };
for (my $at = 0; $at < length $text;) {
    $at += syswrite($w, $text, length($text) - $at, $at) // die "write: $!";
}
for (1 .. 600) { last if -e $mark; select(undef, undef, undef, 0.1) }
-e $mark or die "bash never ran the prefix";
kill "INT", -$$;
close $w;
waitpid($pid, 0);
print $? & 127 ? "signal " . ($? & 127) : "exit " . ($? >> 8);
"#;

/// A Ctrl-C that cuts a `curl | bash` download short ends bash by the signal,
/// so a script around it stops too. The prefix is every line before the final
/// `main "$@"; }`, with one before the whole-file group's `{` that says bash has
/// read that far; nothing in the group has run. Before, the run's INT trap was
/// set at the top level as the script arrived: bash only marked it pending, and
/// the cut script ended with status 0, or mid-function with a syntax error.
#[test]
fn a_ctrl_c_while_the_script_arrives_ends_it_by_the_signal() {
    let scratch = Scratch::new();
    let text = std::fs::read_to_string(install_sh()).expect("install.sh");
    let lines: Vec<&str> = text.lines().collect();
    let open = lines
        .iter()
        .position(|l| *l == "{")
        .expect("install.sh opens its whole-file group");
    let last = lines
        .iter()
        .rposition(|l| *l == "main \"$@\"; }")
        .expect("install.sh ends by calling main");
    let mark = scratch.join("ran");
    let prefix = format!(
        "{}\n: >{}\n{}\n",
        lines[..open].join("\n"),
        q(s(&mark)),
        lines[open..last].join("\n")
    );
    let prefix = scratch.write("prefix.sh", prefix);
    let feed = scratch.write("feed.pl", FEED);
    let mut lib = Lib::new();
    let call = lib.call(
        format!(
            "ATERM_INSTALL_LIBRARY_ONLY=0 /usr/bin/perl {} {} {} {}",
            q(s(&feed)),
            q(s(&prefix)),
            q(s(&mark)),
            q(s(&bash()))
        ),
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].out, "signal 2", "{}", recs[call].err);
}

/// A whole run ends by a Ctrl-C the child it lands in catches: main sets the
/// run's INT trap first. The stand-in `cat` that prints the help catches it and
/// exits 0, which bash, waiting, read as an answer, and the run went on to exit 0.
#[test]
fn a_ctrl_c_a_child_catches_ends_a_whole_run() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(
        "mkdir -p \"$TMPDIR/bin\" && cat >\"$TMPDIR/bin/cat\" <<'EOF'
#!/bin/sh
IFS= read -r first || :
case \"$first\" in
'usage: install.sh'*) trap 'exit 0' INT; kill -s INT 0 ;;
*) printf '%s\\n' \"$first\"; exec /bin/cat ;;
esac
exit 0
EOF
chmod +x \"$TMPDIR/bin/cat\"",
    );
    let call = lib.call(
        format!(
            "PATH=\"$TMPDIR/bin:$PATH\" ATERM_INSTALL_LIBRARY_ONLY=0 {}",
            group_run(&format!("{} --help", q(s(&install_sh()))))
        ),
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[call].out, "signal 2", "{}", recs[call].err);
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

/// A dry run makes no folder, so `ensure_dirs_writable` answers for one still to be made
/// as `mkdir -p` then does: by the nearest part of the way that is there. A folder this
/// account can write takes it, through a link or not; a file, a folder it cannot write,
/// or a link that leads nowhere, standing where the folder would or above it, does not.
#[test]
fn a_dry_run_answers_for_a_folder_still_to_be_made_as_mkdir_does() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    lib.step(
        "answers() {\n\
         \tlocal dry=made real=made before\n\
         \tbefore=\"$(ls -lAR \"$p\")\"\n\
         \tDRY_RUN=1; ensure_dirs_writable \"$1\" || dry=refused; DRY_RUN=0\n\
         \t[[ \"$(ls -lAR \"$p\")\" == \"$before\" ]] || dry=\"$dry, changing $p\"\n\
         \tensure_dirs_writable \"$1\" || real=refused\n\
         \techo \"$dry $real\"\n\
         }",
    );
    let rows = [
        (
            "a folder below one it can write",
            "true",
            "new/deep",
            "made",
        ),
        (
            "through a link to one it can write",
            "mkdir real && ln -s real l",
            "l/new",
            "made",
        ),
        (
            "below a link that leads nowhere",
            "ln -s gone dl",
            "dl/new",
            "refused",
        ),
        (
            "at a link that leads nowhere",
            "ln -s gone dl",
            "dl",
            "refused",
        ),
        ("below a file", ": >f", "f/new", "refused"),
        (
            "below a folder it cannot write",
            "mkdir ro && chmod 555 ro",
            "ro/new",
            "refused",
        ),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(n, (_, setup, path, _))| {
            lib.call(
                format!(
                    "( p=\"$TMPDIR/p{n}\"; mkdir \"$p\" && cd \"$p\" && {setup} && \
                     answers \"$p/{path}\"; s=$?; chmod -R u+w \"$p\"; exit $s )"
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for ((label, _, _, want), i) in rows.iter().zip(calls) {
        assert_eq!(recs[i].rc, 0, "{label}: {}", recs[i].err);
        assert_eq!(
            recs[i].out.trim(),
            format!("{want} {want}"),
            "{label}: the dry run's answer, then mkdir -p's"
        );
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

/// A dry run over a config it cannot read still plans, and says so: the plan's
/// awk failed, and its bare assignment ended the dry run under `set -e`.
#[cfg(target_os = "macos")]
#[test]
fn a_dry_run_over_a_config_it_cannot_read_still_plans() {
    let scratch = Scratch::new();
    let fresh = scratch.home("unread");
    let cfg = fresh.home.join(".config/aterm/aterm.toml");
    write(&cfg, "[packages]\nauto_install = true\n");
    let mode = |m: u32| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(m)).expect("chmod");
    };
    mode(0o000);
    let dry = dry_run(&scratch, &fresh, &["--dry-run", "--no-toolchain"]);
    mode(0o600);
    let plan = dry.run.all();
    assert_eq!(dry.run.code(), 0, "{plan}");
    assert!(
        plan.contains(&format!("config:   cannot read {}", s(&cfg))),
        "{plan}"
    );
}

/// A dry run says what the run does with the config: that it creates the file or
/// adds the key exactly where the run then writes it, and that it cannot write it
/// exactly where the run says to set the key by hand. Both ask the open the run's
/// write makes (config_write_opens): a read-write one for the insert under a
/// [packages] table, an appending one for the append of the table, the folder for
/// a create. The dry run said it adds the key whatever the file's mode; asked by
/// -w, it said it cannot write a file an ACL lets it append to (deny write), which
/// the append wrote, and that it adds the key to one an ACL bars appending to
/// (deny append) and to a folder at the path, where the run wrote nothing. Each
/// row is a plan, what stands in the way (a mode, macOS's chmod +a, a folder), and
/// whether the run writes the key; the dry run must say the same.
#[cfg(target_os = "macos")]
#[test]
fn a_dry_run_says_what_the_run_does_with_the_config() {
    const INSERT: &str = "[packages]\nkeep = 1\n";
    const APPEND: &str = "x = 1\n";
    const DENY_WRITE: &str = "chmod +a \"$(id -un) deny write\" \"$cfg\"";
    const DENY_APPEND: &str = "chmod +a \"$(id -un) deny append\" \"$cfg\"";
    let rows: [(&str, Option<&str>, &str, bool); 11] = [
        ("insert", Some(INSERT), "", true),
        ("insert-444", Some(INSERT), "chmod 444 \"$cfg\"", false),
        ("insert-deny-write", Some(INSERT), DENY_WRITE, false),
        ("insert-deny-append", Some(INSERT), DENY_APPEND, true),
        ("append", Some(APPEND), "", true),
        ("append-444", Some(APPEND), "chmod 444 \"$cfg\"", false),
        ("append-deny-write", Some(APPEND), DENY_WRITE, true),
        ("append-deny-append", Some(APPEND), DENY_APPEND, false),
        ("folder", None, "mkdir \"$cfg\"", false),
        ("create", None, "", true),
        ("create-shut", None, "chmod 555 \"${cfg%/*}\"", false),
    ];
    let scratch = Scratch::new();
    let homes: Vec<Home> = rows.iter().map(|(row, ..)| scratch.home(row)).collect();
    let cfgs: Vec<String> = homes
        .iter()
        .map(|h| s(&h.home.join(".config/aterm/aterm.toml")).to_string())
        .collect();
    let mut setup = Lib::new();
    for ((_, text, guard, _), cfg) in rows.iter().zip(&cfgs) {
        let text = text.map_or(String::new(), |t| format!("printf {} >\"$cfg\" && ", q(t)));
        setup.step(format!(
            "cfg={}; mkdir -p \"${{cfg%/*}}\" && {text}{}",
            q(cfg),
            if guard.is_empty() { ":" } else { guard }
        ));
    }
    setup.run(&scratch);
    let said: Vec<String> = homes
        .iter()
        .map(|home| {
            let plan = dry_run(&scratch, home, &["--dry-run", "--no-toolchain"])
                .run
                .all();
            plan.lines()
                .find(|l| l.starts_with("  config:"))
                .unwrap_or(&plan)
                .to_string()
        })
        .collect();
    let mut lib = Lib::new();
    let calls: Vec<usize> = cfgs
        .iter()
        .map(|cfg| {
            lib.call(
                format!(
                    "cfg={}; s=0; persist_no_toolchain \"$cfg\" >/dev/null 2>&1 || s=$?; \
                     chmod 755 \"${{cfg%/*}}\"; [[ -d \"$cfg\" ]] || chmod -N \"$cfg\" 2>/dev/null; \
                     echo \"$s $([[ -f \"$cfg\" ]] && grep -q '^auto_install = false$' \"$cfg\" && echo key || echo none)\"",
                    q(cfg)
                ),
                &[],
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for (((row, _, _, writes), said), call) in rows.iter().zip(&said).zip(calls) {
        let dry = if said.contains("cannot write") {
            false
        } else {
            assert!(
                said.contains("config:   adds") || said.contains("config:   creates"),
                "{row}: {said}"
            );
            true
        };
        let ran = recs[call].out == "0 key";
        assert!(
            ran || recs[call].out == "1 none",
            "{row}: {}: {}",
            recs[call].out,
            recs[call].err
        );
        assert_eq!(
            (dry, ran),
            (*writes, *writes),
            "{row}: the dry run said {said:?}, the run {}",
            recs[call].out
        );
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
