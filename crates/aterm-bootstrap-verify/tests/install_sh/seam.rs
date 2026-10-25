// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifier pin and seam tests for `install.sh`.

use std::path::{Path, PathBuf};

use aterm_bootstrap_verify::pin::{self, Pin};

use crate::chain::sha256;
use crate::support::{
    Lib, Scratch, bash, hermetic, install_sh, q, read, root, run, s, symlink, system_path, tree,
    unpinned_install_sh, write,
};
use crate::verifier::under_test;

const STAND_IN: &str = "#!/bin/sh\necho \"ran: $*\" >>\"${STUB_RAN:?}\"\n\
                        [ \"$1\" = --store-path ] && exit 0\n\
                        echo 'install.sh: Linux verification refused: a stand-in' >&2\nexit 1\n";

const PINNED_REPO: &str = "pinned-org/aterm";
const PINNED_TAG: &str = "v0.118.0";

const NO_NETWORK: &str = "#!/bin/sh\necho \"$*\" >>\"${STUB_CURL:?}\"\nexit 7\n";

fn said(rec: &crate::support::Rec) -> String {
    format!("rc {} out {:?} err {:?}", rec.rc, rec.out, rec.err)
}

fn pinned(digest: &str) -> Pin {
    Pin {
        repo: PINNED_REPO.into(),
        tag: PINNED_TAG.into(),
        x86_64: digest.into(),
        aarch64: String::new(),
    }
}

fn set(pin: &Pin) -> String {
    pin.lines().replace('\n', "; ")
}

#[test]
fn install_sh_carries_the_pin_its_record_names() {
    let text = read(&install_sh());
    let record = std::fs::read_to_string(root().join(pin::RECORD)).ok();
    let ledger = read(&root().join("RELEASES.ledger"));
    let pin = pin::check(&text, record.as_deref(), &ledger).unwrap_or_else(|why| panic!("{why}"));
    assert!(
        text.contains(&pin.lines()),
        "the four lines, together and in order"
    );
    assert!(
        text.contains(&format!(
            "release_asset_size \"$LINUX_VERIFIER_TAG\" \"$at\" {})",
            pin::MAX_ASSET
        )),
        "install.sh downloads no verifier larger than a release may publish"
    );
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let call = lib.call(
        "printf '%s|%s|%s|%s' \"$LINUX_VERIFIER_REPO\" \"$LINUX_VERIFIER_TAG\" \"$LINUX_VERIFIER_SHA256_X86_64\" \"$LINUX_VERIFIER_SHA256_AARCH64\"",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(
        recs[call].out,
        format!("{}|{}|{}|{}", pin.repo, pin.tag, pin.x86_64, pin.aarch64)
    );
}

#[test]
fn the_seam_runs_only_the_file_it_is_held_to() {
    let scratch = Scratch::new();
    let ran = scratch.join("ran.log");
    let curl = scratch.join("curl.log");
    let stand_in = scratch.exe("seam/verifier", STAND_IN);
    let digest = sha256(&std::fs::read(&stand_in).expect("the stand-in"));
    let other = sha256(b"another file");
    let alias = scratch.join("seam/alias");
    symlink(&stand_in, &alias);
    let built = under_test();
    let built_digest = sha256(&std::fs::read(&built).expect("the verifier"));
    let shim = scratch.mkdir("shim");
    scratch.exe("shim/python3", "#!/bin/sh\nexit 1\n");
    scratch.exe("shim/curl", NO_NETWORK);
    let bare = scratch.mkdir("bare");
    let mut lib = Lib::new();
    lib.env("STUB_RAN", s(&ran))
        .env("STUB_CURL", s(&curl))
        .env("PATH", &format!("{}:{}", s(&shim), system_path()));
    let ready = |lib: &mut Lib, exe: &Path, sha: &str| {
        lib.call(
            format!(
                "ATERM_INSTALL_VERIFIER={} ATERM_INSTALL_VERIFIER_SHA256={} linux_verifier_ready x86_64",
                q(s(exe)),
                q(sha)
            ),
            &["LINUX_VERIFIER", "LINUX_VERIFIER_WHY"],
        )
    };
    let refuse = |lib: &mut Lib, exe: &Path| {
        lib.call(
            format!(
                "ATERM_INSTALL_VERIFIER={} linux_release_verify --store-path /nowhere",
                q(s(exe))
            ),
            &[],
        )
    };
    let good = ready(&mut lib, &stand_in, &digest);
    let runs = lib.call("linux_release_verify --store-path /nowhere", &[]);
    let refusals: Vec<(usize, usize, String)> = [
        (
            stand_in.clone(),
            other.clone(),
            format!("has SHA-256 {digest}, not the pinned {other}"),
        ),
        (
            built.clone(),
            other.clone(),
            format!("has SHA-256 {built_digest}, not the pinned {other}"),
        ),
        (
            stand_in.clone(),
            String::new(),
            "has no pinned SHA-256 to be held to".into(),
        ),
        (
            stand_in.clone(),
            digest.to_uppercase(),
            "has no pinned SHA-256 to be held to".into(),
        ),
        (
            alias.clone(),
            digest.clone(),
            "is not a regular file".into(),
        ),
        (
            scratch.join("seam/absent"),
            digest.clone(),
            "is not a regular file".into(),
        ),
    ]
    .into_iter()
    .map(|(exe, sha, why)| {
        let call = ready(&mut lib, &exe, &sha);
        (call, refuse(&mut lib, &exe), why)
    })
    .collect();
    let unread = |lib: &mut Lib, pin: &Pin| {
        lib.call(
            format!(
                "{}ATERM_INSTALL_VERIFIER={} ATERM_INSTALL_VERIFIER_SHA256={digest} PATH={} linux_verifier_ready x86_64",
                set(pin),
                q(s(&stand_in)),
                q(s(&bare))
            ),
            &["LINUX_VERIFIER", "LINUX_VERIFIER_WHY"],
        )
    };
    let sumless = unread(&mut lib, &Pin::default());
    let sumless_pinned = unread(&mut lib, &pinned(&digest));
    let whole = |lib: &mut Lib, pin: &Pin| {
        lib.call(
            format!(
                "ATERM_INSTALL_LIBRARY_ONLY=0; {}ATERM_INSTALL_VERIFIER={} ATERM_INSTALL_VERIFIER_SHA256={digest} linux_verifier_ready x86_64",
                set(pin),
                q(s(&stand_in))
            ),
            &["LINUX_VERIFIER", "LINUX_VERIFIER_WHY"],
        )
    };
    let whole_unpinned = whole(&mut lib, &Pin::default());
    let whole_pinned = whole(&mut lib, &pinned(&digest));
    let recs = lib.run(&scratch);
    assert_eq!(recs[good].rc, 0, "{}", said(&recs[good]));
    assert_eq!(recs[good].var("LINUX_VERIFIER"), s(&stand_in));
    assert_eq!(recs[runs].rc, 0, "{}", said(&recs[runs]));
    for (call, dispatched, why) in &refusals {
        let rec = &recs[*call];
        assert_eq!(rec.rc, 2, "{why}: {}", said(rec));
        assert_eq!(rec.var("LINUX_VERIFIER"), "", "{why}");
        let words = rec.var("LINUX_VERIFIER_WHY");
        assert!(
            words.starts_with("the bootstrap verifier ")
                && words.contains(why.as_str())
                && words.ends_with(" — refusing to run it"),
            "{why}: {words}"
        );
        assert_eq!(recs[*dispatched].rc, 1, "{}", said(&recs[*dispatched]));
        assert_eq!(
            recs[*dispatched].err,
            "install.sh: Linux verification refused: the pinned bootstrap verifier is not ready"
        );
    }
    for call in [sumless, sumless_pinned] {
        let rec = &recs[call];
        assert_eq!(rec.rc, 1, "no SHA-256 reader skips the half: {}", said(rec));
        assert_eq!(rec.var("LINUX_VERIFIER"), "");
        assert_eq!(
            rec.var("LINUX_VERIFIER_WHY"),
            "the pinned bootstrap verifier is held to its SHA-256, and this host has neither \
             sha256sum nor shasum to read one — install coreutils and re-run"
        );
    }
    assert_eq!(
        read(&ran).lines().collect::<Vec<_>>(),
        ["ran: --store-path /nowhere"],
        "the held file ran once, and no refused one ran"
    );
    for call in [whole_unpinned, whole_pinned] {
        assert_eq!(
            recs[call].var("LINUX_VERIFIER"),
            "",
            "a whole run reads no seam"
        );
    }
    assert_eq!(
        recs[whole_unpinned].var("LINUX_VERIFIER_WHY"),
        "signed Linux bootstrap needs Python 3.11+ and OpenSSL with Ed25519 support — install them and re-run"
    );
    let name = pin::asset_name(PINNED_TAG.trim_start_matches('v'), "x86_64");
    assert_eq!(
        recs[whole_pinned].var("LINUX_VERIFIER_WHY"),
        format!(
            "could not read the bootstrap verifier {name} in release {PINNED_TAG} of \
             {PINNED_REPO}; retry in a few minutes"
        )
    );
    assert_eq!(
        read(&curl).lines().count(),
        1,
        "only the pinned whole run asked the network, of the stub that fails it: {}",
        read(&curl)
    );
}

#[test]
fn a_pinned_verifier_is_fetched_held_to_its_digest_and_removed() {
    let scratch = Scratch::new();
    let ran = scratch.join("ran.log");
    let fetched_log = scratch.join("fetched.log");
    let asset = scratch.write("host/asset", STAND_IN);
    let digest = sha256(STAND_IN.as_bytes());
    let name = pin::asset_name(PINNED_TAG.trim_start_matches('v'), "x86_64");
    let mut lib = Lib::new();
    lib.env("STUB_RAN", s(&ran))
        .env("STUB_ASSET", s(&asset))
        .env("STUB_FETCHED", s(&fetched_log));
    lib.step(format!(
        "release_asset_size() {{ [[ \"$REPO_SLUG $1 $2 $3\" == {} ]] || return 1; wc -c <\"$STUB_ASSET\" | tr -d ' '; }}\n\
         download_release_asset() {{ echo \"$REPO_SLUG $1 $2\" >>\"$STUB_FETCHED\"; cat \"$STUB_ASSET\" >\"$4\"; }}\n\
         linux_release_verify_python() {{ echo \"embedded $*\"; }}\n\
         REPO_SLUG=channel-org/aterm\n{}",
        q(&format!("{PINNED_REPO} {PINNED_TAG} {name} 67108864")),
        set(&pinned(""))
    ));
    let vars = [
        "LINUX_VERIFIER",
        "LINUX_VERIFIER_DIR",
        "LINUX_VERIFIER_WHY",
        "LINUX_VERIFIER_PLANNED",
        "LINUX_VERIFIER_PLANNED_SIZE",
        "REPO_SLUG",
    ];
    let planned = lib.call(
        format!("LINUX_VERIFIER_SHA256_X86_64={digest}; DRY_RUN=1 linux_verifier_ready x86_64"),
        &vars,
    );
    let fetched = lib.call("linux_verifier_ready x86_64", &vars);
    let private = lib.call(
        "[[ -x \"$LINUX_VERIFIER\" && ! -L \"$LINUX_VERIFIER\" ]] && ls -ld \"$LINUX_VERIFIER_DIR\" \"$LINUX_VERIFIER\" | cut -c1-10",
        &[],
    );
    let runs = lib.call("linux_release_verify --store-path /nowhere", &[]);
    let removed = lib.call(
        "d=\"$LINUX_VERIFIER_DIR\"; linux_verifier_cleanup; [[ ! -e \"$d\" ]]",
        &vars,
    );
    let other = sha256(b"another build");
    let mismatched = lib.call(
        format!("LINUX_VERIFIER_SHA256_X86_64={other}; linux_verifier_ready x86_64"),
        &vars,
    );
    let refused = lib.call(
        "LINUX_ARCH=x86_64 linux_release_verify --store-path /nowhere",
        &[],
    );
    let unpinned = lib.call(
        "LINUX_ARCH=aarch64 linux_release_verify --store-path /nowhere",
        &[],
    );
    let sizeless = lib.call(
        format!(
            "LINUX_VERIFIER_SHA256_X86_64={digest}; release_asset_size() {{ return 1; }}; linux_verifier_ready x86_64"
        ),
        &vars,
    );
    let undelivered = lib.call(
        "release_asset_size() { echo 5; }; download_release_asset() { return 1; }; linux_verifier_ready x86_64",
        &vars,
    );
    let recs = lib.run(&scratch);

    let rec = &recs[planned];
    assert_eq!(rec.rc, 0, "{}", said(rec));
    assert_eq!(rec.var("LINUX_VERIFIER"), "", "a dry run fetches nothing");
    assert_eq!(rec.var("LINUX_VERIFIER_DIR"), "");
    assert_eq!(rec.var("LINUX_VERIFIER_PLANNED"), name);
    assert_eq!(
        rec.var("LINUX_VERIFIER_PLANNED_SIZE"),
        STAND_IN.len().to_string()
    );

    let rec = &recs[fetched];
    assert_eq!(rec.rc, 0, "{}", said(rec));
    assert_eq!(rec.var("LINUX_VERIFIER_PLANNED"), "");
    assert_eq!(
        rec.var("REPO_SLUG"),
        "channel-org/aterm",
        "the channel is kept"
    );
    let dir = rec.var("LINUX_VERIFIER_DIR");
    let tmp = scratch.join("tmp");
    assert!(
        Path::new(dir).parent() == Some(tmp.as_path())
            && Path::new(dir)
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("aterm-verifier.")),
        "{dir}"
    );
    assert_eq!(rec.var("LINUX_VERIFIER"), format!("{dir}/{name}"));
    assert_eq!(
        recs[private].out,
        "drwx------\n-rwx------",
        "{}",
        said(&recs[private])
    );
    assert_eq!(recs[runs].rc, 0, "{}", said(&recs[runs]));
    assert_eq!(recs[removed].rc, 0, "{}", said(&recs[removed]));
    assert_eq!(recs[removed].var("LINUX_VERIFIER"), "");
    assert_eq!(recs[removed].var("LINUX_VERIFIER_DIR"), "");

    let rec = &recs[mismatched];
    assert_eq!(rec.rc, 2, "{}", said(rec));
    assert_eq!(rec.var("LINUX_VERIFIER"), "");
    assert_eq!(
        rec.var("LINUX_VERIFIER_WHY"),
        format!(
            "the bootstrap verifier {}/{name} has SHA-256 {digest}, not the pinned {other} — refusing to run it",
            rec.var("LINUX_VERIFIER_DIR")
        )
    );
    assert_eq!(recs[refused].rc, 1, "{}", said(&recs[refused]));
    assert_eq!(
        recs[refused].err,
        "install.sh: Linux verification refused: the pinned bootstrap verifier is not ready"
    );
    assert_eq!(recs[unpinned].rc, 0, "{}", said(&recs[unpinned]));
    assert_eq!(recs[unpinned].out, "embedded --store-path /nowhere");
    assert_eq!(recs[sizeless].rc, 1, "{}", said(&recs[sizeless]));
    assert_eq!(
        recs[sizeless].var("LINUX_VERIFIER_WHY"),
        format!(
            "could not read the bootstrap verifier {name} in release {PINNED_TAG} of \
             {PINNED_REPO}; retry in a few minutes"
        )
    );
    assert_eq!(recs[undelivered].rc, 1, "{}", said(&recs[undelivered]));
    assert_eq!(
        recs[undelivered].var("LINUX_VERIFIER_WHY"),
        format!(
            "could not download the bootstrap verifier {name} from release {PINNED_TAG} of \
             {PINNED_REPO}; retry in a few minutes"
        )
    );
    assert_eq!(
        read(&fetched_log).lines().collect::<Vec<_>>(),
        vec![format!("{PINNED_REPO} {PINNED_TAG} {name}"); 2],
        "fetched from the pin's repository, twice: never by the dry run"
    );
    assert_eq!(
        read(&ran).lines().collect::<Vec<_>>(),
        ["ran: --store-path /nowhere"],
        "the held file ran once, and the mismatched one never"
    );
    let left: Vec<String> = tree(&tmp)
        .into_iter()
        .filter(|entry| entry.starts_with("aterm-verifier."))
        .collect();
    assert!(
        left.is_empty(),
        "the exit trap removes what was fetched: {left:?}"
    );
}

const HOST: &str = r#"#!/bin/sh
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
VERIFIER="https://github.com/pinned-org/aterm/releases/download/v0.118.0/aterm-bootstrap-verify-0.118.0-linux-x86_64"
ELF="$BASE/download/v9.9.9/aterm-9.9.9-linux-x86_64"
sized() { printf 'HTTP/2 302\r\nlocation: https://objects.example/a\r\n\r\nHTTP/2 200\r\ncontent-length: %s\r\n\r\n' "$1"; }
case "$head$follow $url" in
"10 $BASE/latest/download/aterm-appcast.toml")
	printf '%s' "$fmt" | sed "s#%{http_code}#302#; s#%{redirect_url}#$BASE/download/v9.9.9/aterm-appcast.toml#" ;;
"11 $VERIFIER") sized "$(wc -c <"${STUB_ASSET:?}" | tr -d ' ')" ;;
"01 $VERIFIER") cat "$STUB_ASSET" ;;
"10 $ELF")
	if [ -n "${STUB_PUBLISHED:-}" ]; then code=302; else code=404; fi
	printf '%s' "$fmt" | sed "s#%{http_code}#$code#" ;;
"11 $ELF") [ -n "${STUB_PUBLISHED:-}" ] && sized 12000000 ;;
*) echo "curl stub: unexpected request: $head$follow $url" >&2; exit 64 ;;
esac
"#;

struct Piped {
    code: i32,
    out: String,
    requests: Vec<String>,
    ran: Vec<String>,
    left: Vec<String>,
}

fn piped(
    scratch: &Scratch,
    name: &str,
    script: &Path,
    extra: &[(&str, &str)],
    args: &str,
) -> Piped {
    let shim = scratch.mkdir("whole-shim");
    scratch.stand_in("whole-shim/curl", HOST);
    scratch.stand_in(
        "whole-shim/gh",
        "#!/bin/sh\necho \"gh stub: the installer ran gh $*\" >&2\nexit 64\n",
    );
    scratch.stand_in(
        "whole-shim/uname",
        "#!/bin/sh\ncase \"$1\" in -m) echo x86_64 ;; *) echo Linux ;; esac\n",
    );
    scratch.stand_in("whole-shim/python3", "#!/bin/sh\nexit 1\n");
    let asset = scratch.write("whole-host/asset", STAND_IN);
    let home = scratch.home(name);
    let log = scratch.join(&format!("{name}/requests.log"));
    let ran = scratch.join(&format!("{name}/ran.log"));
    write(&log, "");
    let path = format!("{}:{}", s(&shim), system_path());
    let mut env = vec![
        ("PATH", path.as_str()),
        ("STUB_LOG", s(&log)),
        ("STUB_RAN", s(&ran)),
        ("STUB_ASSET", s(&asset)),
    ];
    env.extend_from_slice(extra);
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.arg("-c")
        .arg(format!(
            "exec {} -s -- {args} <{}",
            q(s(&bash())),
            q(s(script))
        ))
        .env_clear();
    for (key, value) in hermetic(&home, &env) {
        cmd.env(key, value);
    }
    let done = run(&mut cmd);
    Piped {
        code: done.code(),
        out: done.all(),
        requests: read(&log).lines().map(str::to_string).collect(),
        ran: read(&ran).lines().map(str::to_string).collect(),
        left: tree(&home.tmp)
            .into_iter()
            .filter(|entry| entry.starts_with("aterm-verifier."))
            .collect(),
    }
}

fn pinned_copy(scratch: &Scratch, name: &str, pin: &Pin) -> PathBuf {
    let text = pin
        .rewrite(&read(&install_sh()))
        .unwrap_or_else(|why| panic!("{why}"));
    scratch.write(name, text)
}

#[test]
fn a_whole_linux_run_holds_the_pinned_verifier_or_keeps_the_embedded_one() {
    let scratch = Scratch::new();
    let digest = sha256(STAND_IN.as_bytes());
    let held = pinned_copy(&scratch, "held/install.sh", &pinned(&digest));
    let other = pinned_copy(
        &scratch,
        "other/install.sh",
        &pinned(&sha256(b"another build")),
    );
    let verifier = "https://github.com/pinned-org/aterm/releases/download/v0.118.0/aterm-bootstrap-verify-0.118.0-linux-x86_64";
    let absent = "the latest release v9.9.9 publishes no Linux x86_64 executable \
                  (aterm-9.9.9-linux-x86_64) — nothing to install on this machine yet";

    let run = piped(&scratch, "held", &held, &[], "");
    assert_eq!(run.code, 1, "{}", run.out);
    assert!(run.out.contains(absent), "{}", run.out);
    assert!(
        run.requests.contains(&format!("11 {verifier}")),
        "{:?}",
        run.requests
    );
    assert!(
        run.requests.contains(&format!("01 {verifier}")),
        "{:?}",
        run.requests
    );
    assert_eq!(run.ran.len(), 1, "{:?}\n{}", run.ran, run.out);
    assert!(
        run.ran[0].starts_with("ran: --store-path /"),
        "{:?}",
        run.ran
    );
    assert!(run.left.is_empty(), "{:?}", run.left);

    let run = piped(&scratch, "other", &other, &[], "");
    assert_eq!(run.code, 1, "{}", run.out);
    assert!(
        run.out
            .contains("install.sh: REFUSED the app: the bootstrap verifier ")
            && run
                .out
                .contains(&format!("has SHA-256 {digest}, not the pinned ")),
        "{}",
        run.out
    );
    assert!(run.ran.is_empty(), "{:?}", run.ran);
    assert!(run.left.is_empty(), "{:?}", run.left);

    let stand_in = scratch.exe("seam/verifier", STAND_IN);
    let unpinned = unpinned_install_sh(&scratch, "embedded/install.sh");
    let run = piped(
        &scratch,
        "embedded",
        &unpinned,
        &[
            ("ATERM_INSTALL_VERIFIER", s(&stand_in)),
            ("ATERM_INSTALL_VERIFIER_SHA256", &digest),
        ],
        "",
    );
    assert!(
        run.out.contains(
            "signed Linux bootstrap needs Python 3.11+ and OpenSSL with Ed25519 support — install them and re-run"
        ),
        "{}",
        run.out
    );
    assert!(
        !run.requests.iter().any(|r| r.contains("bootstrap-verify")),
        "{:?}",
        run.requests
    );
    assert!(run.ran.is_empty(), "the seam is a suite's: {:?}", run.ran);

    let run = piped(
        &scratch,
        "plan",
        &held,
        &[("STUB_PUBLISHED", "1")],
        "--dry-run",
    );
    assert_eq!(run.code, 0, "{}", run.out);
    let kb = STAND_IN.len().div_ceil(1000);
    assert!(
        run.out.contains(&format!(
            "  verifier: aterm-bootstrap-verify-0.118.0-linux-x86_64 from {PINNED_REPO} \
             {PINNED_TAG} — {kb} kB; a real install fetches it into a private temp dir, runs it \
             only if its SHA-256 is the pinned one (first on "
        )),
        "{}",
        run.out
    );
    assert!(
        run.out
            .contains("  note:     this dry-run reads sizes only;"),
        "{}",
        run.out
    );
    assert!(
        run.requests.contains(&format!("11 {verifier}"))
            && !run.requests.contains(&format!("01 {verifier}")),
        "the dry run sizes the verifier and fetches nothing: {:?}",
        run.requests
    );
    assert!(
        run.ran.is_empty(),
        "the dry run runs nothing: {:?}",
        run.ran
    );
    assert!(run.left.is_empty(), "{:?}", run.left);
    for wrong in ["curl stub:", "gh stub:"] {
        assert!(!run.out.contains(wrong), "{}", run.out);
    }
}

#[test]
fn an_install_removes_the_fetched_verifier_when_it_ends() {
    let scratch = Scratch::new();
    let pin = pinned(&sha256(STAND_IN.as_bytes()));
    let held = pinned_copy(&scratch, "held/install.sh", &pin);
    let text = read(&held);
    let forgetful = "install_linux_app_cleanup() {\n\tset +e\n\ttrap '' INT HUP TERM QUIT\n\tlinux_verifier_cleanup\n\trm -rf \"$TMP\"\n}\n";
    assert_eq!(
        text.matches(forgetful).count(),
        1,
        "install_linux_app's cleanup, as the control breaks it"
    );
    let broken = scratch.write(
        "broken/install.sh",
        text.replacen(
            forgetful,
            "install_linux_app_cleanup() {\n\tset +e\n\ttrap '' INT HUP TERM QUIT\n\trm -rf \"$TMP\"\n}\n",
            1,
        ),
    );
    let installing = "install.sh: installing alabsystems/aterm v9.9.9 (linux-x86_64)";
    for (name, script, left) in [("held", &held, false), ("broken", &broken, true)] {
        let run = piped(&scratch, name, script, &[("STUB_PUBLISHED", "1")], "");
        assert_ne!(run.code, 0, "{name}: {}", run.out);
        assert!(run.out.contains(installing), "{name}: {}", run.out);
        assert_eq!(run.ran.len(), 1, "{name}: {:?}\n{}", run.ran, run.out);
        assert_eq!(
            !run.left.is_empty(),
            left,
            "{name}: {:?}\n{}",
            run.left,
            run.out
        );
    }
}
