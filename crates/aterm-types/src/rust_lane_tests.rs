// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

fn commands(spellings: &[Spelling]) -> Vec<&str> {
    spellings.iter().map(|s| s.command.as_str()).collect()
}

/// THE INTERIM GUARD'S 37 CASES, ported verbatim and in its order: the
/// orchestrator's `~/.claude/hooks/trust-toolchain-guard.py` and its
/// `guard_test.py` — eighteen commands it blocked and nineteen it let through.
/// This reader is what a guard calls in that script's place (`aterm pkg lane`), so
/// every one of these verdicts is the floor, with ONE named exception
/// ([`INTERIM_ALLOW_REFUSED_BY_DESIGN`]).
///
/// The first port (2026-09-24) held the 33 cases the script had then. On
/// 2026-09-25 the script gained its by-path exemption and four PATH-FORM cases
/// (the first two of each list), the only four that exercise it, and the port
/// did not follow — so until 2026-09-28 it still said "33, verbatim" while
/// `aterm pkg lane` refused the trust repository's own
/// `build/<host>/stage0/bin/rustc -Vv` as stock (review of 2026-09-27). A
/// path-form case needs a filesystem: [`interim_guard_machine`] answers, for each
/// directory as the case writes it, what that directory held on the owner's Mac
/// when measured (2026-09-28).
const INTERIM_BLOCK: &[&str] = &[
    "/usr/local/bin/cargo build",
    "~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustc -V",
    "cargo +1.97.1 test -p x",
    "cd /a && CARGO_BUILD_JOBS=4 cargo clippy --all-targets",
    "cargo fmt --check",
    "rustfmt src/lib.rs",
    "rustup run 1.97.1 rustfmt a.rs",
    "time cargo build",
    "x=$(rustc --version)",
    "echo `rustc -V`",
    "bash -c 'cd x && cargo test'",
    "for d in a b; do cargo test -p $d; done",
    "if true; then rustc a.rs; fi",
    "(cd x; cargo check)",
    "FOO=1 /usr/bin/env cargo build",
    "echo \"$(cargo metadata)\"",
    "ls | xargs -n1 echo; cargo tree",
    "git commit -m \"fix: replace `cargo fmt` with trustfmt\"",
];

const INTERIM_ALLOW: &[&str] = &[
    "/Users//example/trust/build/aarch64-apple-darwin/stage0/bin/rustc -Vv",
    "build/aarch64-apple-darwin/stage0/bin/cargo --version",
    "targo --unverified test -p x",
    "targo tippy -p x",
    "targo fmt --check",
    "trustfmt a.rs",
    "grep -rn \"cargo clippy\" scripts",
    "git log -S cargo",
    "./x.py test tests/ui",
    "ATERM_STOCK_REASON='measure stock parity' cargo +1.97.1 test",
    "echo cargo",
    "ls ~/.cargo/registry",
    "git commit -m 'docs: stop saying `cargo +1.97.1`; use `targo tippy`'",
    "python3.12 - <<'EOF'\nprint('`cargo +1.97.1` is stock')\ncargo build\nEOF\necho done",
    "cat > f.md <<EOF\ncargo test\nEOF",
    "echo hi # cargo test",
    "sed -i '' 's/cargo clippy/targo tippy/' a.sh",
    "rg 'rustc --version' -n",
    "python3.12 -c \"print('cargo')\"",
];

/// The one case of [`INTERIM_ALLOW`] this reader refuses ON PURPOSE: a Trust
/// toolchain's compatibility `cargo` by path. The interim script let `rustc`,
/// `rustdoc`, `cargo` and `rustfmt` through by path in any directory holding
/// `trustc` or `targo`; this reader lets through `rustc` and `rustdoc` beside
/// their own twins ([`COMPAT_TWINS`]) and keeps refusing `cargo` there, spelled
/// with THAT directory's `targo`. Measured 2026-09-28 on the installed Trust
/// toolchain (`~/toolchains/trust-45040366/bin`): its `cargo` is byte-identical
/// to its `targo`, yet run as `cargo` it answers `--unverified` with "unexpected
/// argument" — upstream cargo's surface, which names no lane. A choice put to the
/// owner with the guard's wiring, not an accident of the port.
const INTERIM_ALLOW_REFUSED_BY_DESIGN: &str =
    "build/aarch64-apple-darwin/stage0/bin/cargo --version";

/// The directory probe of [`verdict_in`] for the interim cases: what each
/// directory, AS THE CASE WRITES IT, held on the owner's Mac on 2026-09-28
/// (`ls`; the relative form is the same directory read under the trust
/// checkout, where the script's own measurement ran): the trust `stage0/bin`
/// (no `rustdoc`, no `rustfmt`), rustup's `1.97.1` (no Trust name), and
/// `/usr/local/bin`, which holds none of these names.
fn interim_guard_machine(dir: &str, name: &str) -> bool {
    const STAGE0: &[&str] = &[
        "cargo",
        "rustc",
        "targo",
        "targo-fmt",
        "targo-tippy",
        "targo-trust",
        "tippy",
        "tippy-driver",
        "trust-analyzer",
        "trust-gdb",
        "trust-gdbgui",
        "trust-lldb",
        "trustc",
        "trustdoc",
        "trustfmt",
    ];
    const RUSTUP_1_97_1: &[&str] = &[
        "cargo",
        "cargo-clippy",
        "cargo-fmt",
        "clippy-driver",
        "rust-gdb",
        "rust-gdbgui",
        "rust-lldb",
        "rustc",
        "rustdoc",
        "rustfmt",
    ];
    let held: &[&str] = match dir {
        "/Users//example/trust/build/aarch64-apple-darwin/stage0/bin/"
        | "build/aarch64-apple-darwin/stage0/bin/" => STAGE0,
        "~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/" => RUSTUP_1_97_1,
        _ => &[],
    };
    held.contains(&name)
}

/// [`verdict_in`] as a guard reads it: the escaped command goes through.
fn refused_on(machine: &dyn Fn(&str, &str) -> bool, cmd: &str) -> Option<Hit> {
    match verdict_in(cmd, machine) {
        Verdict::Stock(hit) => Some(hit),
        Verdict::Clean | Verdict::Escaped { .. } => None,
    }
}

#[test]
fn the_interim_guards_thirty_seven_cases_hold() {
    assert_eq!((INTERIM_BLOCK.len(), INTERIM_ALLOW.len()), (18, 19));
    for cmd in INTERIM_BLOCK {
        assert!(
            refused_on(&interim_guard_machine, cmd).is_some(),
            "MISSED {cmd:?}"
        );
    }
    for cmd in INTERIM_ALLOW
        .iter()
        .filter(|c| **c != INTERIM_ALLOW_REFUSED_BY_DESIGN)
    {
        assert_eq!(
            refused_on(&interim_guard_machine, cmd),
            None,
            "FALSE POSITIVE {cmd:?}"
        );
    }
    // The named exception: refused, with the `targo` beside that `cargo` — never
    // "stock Rust", and never whichever `targo` is first on PATH.
    assert!(INTERIM_ALLOW.contains(&INTERIM_ALLOW_REFUSED_BY_DESIGN));
    let hit = refused_on(&interim_guard_machine, INTERIM_ALLOW_REFUSED_BY_DESIGN)
        .expect("a Trust toolchain's compatibility cargo names no lane");
    assert_eq!(
        hit.toolchain_dir.as_deref(),
        Some("build/aarch64-apple-darwin/stage0/bin/")
    );
    assert_eq!(
        commands(&hit.instead),
        ["build/aarch64-apple-darwin/stage0/bin/targo --version"]
    );
    assert!(!refusal(&hit, None).contains("is stock Rust"));
    // The text-only reading knows no directory: every path-form case is read by
    // its name there, which is why a guard reads with `verdict_in`.
    for cmd in INTERIM_BLOCK[..2].iter().chain(&INTERIM_ALLOW[..2]) {
        assert!(check(cmd).is_some(), "{cmd:?}");
    }
}

/// A TRUST TOOLCHAIN'S OWN COMPATIBILITY NAMES BY PATH (review of 2026-09-27):
/// `<dir>/rustc` beside `<dir>/trustc` and `<dir>/rustdoc` beside `<dir>/trustdoc`
/// are that toolchain's compiler and doc tool — `build/<host>/stage2/bin/rustc -vV`
/// is how the trust repository reads the compiler under measurement — so they are
/// never a hit, wherever the shell runs them. `<dir>/cargo` beside `<dir>/targo` is
/// upstream cargo's surface (it refuses `--unverified`), so it stays a hit, spelled
/// with THAT directory's `targo`. Only the twin counts: a `rustdoc` beside a
/// `trustc` alone, a `rustfmt` anywhere, and every bare name are read as before;
/// the text-only [`check`] knows no directory and reads every path by its name.
#[test]
fn a_trust_toolchains_own_rustc_by_path_is_not_stock_and_its_cargo_names_its_targo() {
    // A simulated filesystem: what each directory, as written, holds.
    let dirs: &[(&str, &[&str])] = &[
        (
            "build/aarch64-apple-darwin/stage2/bin/",
            &["trustc", "trustdoc", "targo", "rustc", "rustdoc", "cargo"],
        ),
        (
            "~/toolchains/trust-45040366/bin/",
            &["trustc", "trustdoc", "targo"],
        ),
        ("/opt/trust/bin/", &["trustc", "trustdoc", "targo"]),
        // stage0 of a trust checkout: no trustdoc, so its `rustdoc` is not a twin.
        ("./build/x/stage0/bin/", &["trustc", "targo"]),
        ("/opt/stock/bin/", &["rustc", "cargo", "rustdoc"]),
    ];
    let beside = |dir: &str, name: &str| {
        dirs.iter()
            .any(|(d, files)| *d == dir && files.contains(&name))
    };
    for cmd in [
        "build/aarch64-apple-darwin/stage2/bin/rustc -vV",
        "~/toolchains/trust-45040366/bin/rustc --version",
        "/opt/trust/bin/rustdoc --version",
        "./build/x/stage0/bin/rustc -vV 2>&1 | head -3",
        "bash -c '/opt/trust/bin/rustc -vV'",
        "x=$(/opt/trust/bin/rustc --print sysroot)",
        "timeout 10 /opt/trust/bin/rustc -vV",
        "RUSTC_LOG=info /opt/trust/bin/rustc --edition 2024 a.rs",
        "find . -name '*.rs' -exec /opt/trust/bin/rustc --edition 2024 {} \\;",
    ] {
        assert_eq!(verdict_in(cmd, &beside), Verdict::Clean, "{cmd:?}");
        assert!(check(cmd).is_some(), "the text-only reading: {cmd:?}");
    }
    for cmd in [
        "rustc -vV",
        "/opt/stock/bin/rustc -vV",
        "/opt/stock/bin/rustdoc --version",
        // The twin of rustdoc is trustdoc, not trustc.
        "./build/x/stage0/bin/rustdoc --version",
        // A Trust toolchain ships no compatibility rustfmt.
        "/opt/trust/bin/rustfmt a.rs",
        // The exemption covers its own word only.
        "/opt/trust/bin/rustc -vV && cargo build",
        "/opt/trust/bin/rustc -vV; rustfmt a.rs",
        // A directory the probe was not asked about, or answered no for.
        "$D/rustc -vV",
        "/opt/trust/lib/rustc -vV",
    ] {
        assert!(
            matches!(verdict_in(cmd, &beside), Verdict::Stock(_)),
            "MISSED {cmd:?}"
        );
    }
    // The Trust toolchain's own cargo by path: refused, with its own targo.
    let Verdict::Stock(hit) = verdict_in("/opt/trust/bin/cargo test -p x", &beside) else {
        panic!("a compatibility cargo names no lane");
    };
    assert_eq!(hit.used, "/opt/trust/bin/cargo test");
    assert_eq!(hit.toolchain_dir.as_deref(), Some("/opt/trust/bin/"));
    assert_eq!(
        commands(&hit.instead),
        [
            "/opt/trust/bin/targo trust test -p x",
            "/opt/trust/bin/targo --unverified test -p x"
        ]
    );
    let Verdict::Stock(meta) = verdict_in(
        "build/aarch64-apple-darwin/stage2/bin/cargo metadata --format-version 1",
        &beside,
    ) else {
        panic!("refused");
    };
    assert_eq!(
        commands(&meta.instead),
        ["build/aarch64-apple-darwin/stage2/bin/targo metadata --format-version 1"]
    );
    // A stock cargo by path is spelled with the targo on PATH, as ever.
    let Verdict::Stock(stock) = verdict_in("/opt/stock/bin/cargo build", &beside) else {
        panic!("refused");
    };
    assert_eq!(stock.toolchain_dir, None);
    assert_eq!(stock.used, "cargo build");
    assert_eq!(
        commands(&stock.instead),
        ["targo trust build", "targo --unverified build"]
    );
    // The refusal says what that binary IS — never "stock Rust" — and names no pin:
    // a path runs no rustup proxy, so a pin moves nothing there.
    let pin = Some((Path::new("/r/rust-toolchain.toml"), "1.97.1"));
    let text = refusal(&hit, pin);
    assert!(
        text.starts_with(
            "`/opt/trust/bin/cargo test` is the compatibility `cargo` of the Trust toolchain in \
             /opt/trust/bin/: upstream cargo's surface, not targo's"
        ),
        "{text}"
    );
    assert!(!text.contains("is stock Rust"), "{text}");
    assert!(!text.contains("pins stock"), "{text}");
    assert!(
        text.contains("\n  /opt/trust/bin/targo trust test -p x "),
        "{text}"
    );
    assert!(text.contains("ATERM_STOCK_REASON='<why>'"), "{text}");
    // The escape clears it like any other.
    assert!(matches!(
        verdict_in(
            "ATERM_STOCK_REASON='compat-name smoke' /opt/trust/bin/cargo --version",
            &beside
        ),
        Verdict::Escaped { .. }
    ));
}

/// [`file_beside`] reads a directory the way the shell wrote it: absolute, under
/// the cwd, or under `~/`/`$HOME/`/`${HOME}/`; anything it cannot resolve is not
/// a Trust toolchain.
#[test]
fn file_beside_resolves_a_directory_the_way_the_shell_wrote_it() {
    let root = std::env::temp_dir().join(format!("rust-lane-beside-{}", std::process::id()));
    let home = root.join("home");
    let bin = home.join("tc/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("trustc"), b"").unwrap();
    let abs = format!("{}/", bin.display());
    assert!(file_beside(&abs, "trustc", None, None));
    assert!(!file_beside(&abs, "trustdoc", None, None));
    assert!(file_beside("tc/bin/", "trustc", Some(&home), None));
    assert!(file_beside("./tc/bin/", "trustc", Some(&home), None));
    assert!(
        !file_beside("tc/bin/", "trustc", None, Some(&home)),
        "a relative directory with no cwd to read it under"
    );
    for dir in ["~/tc/bin/", "$HOME/tc/bin/", "${HOME}/tc/bin/"] {
        assert!(file_beside(dir, "trustc", None, Some(&home)), "{dir}");
        assert!(!file_beside(dir, "trustc", None, None), "no home: {dir}");
    }
    for dir in [
        "$D/",
        "~root/tc/bin/",
        "t*/bin/",
        "$SUB/tc/bin/",
        "`pwd`/tc/bin/",
    ] {
        assert!(
            !file_beside(dir, "trustc", Some(&home), Some(&home)),
            "unresolvable: {dir}"
        );
    }
    // A DIRECTORY named trustc is not the compiler.
    std::fs::create_dir_all(root.join("fake/bin/trustc")).unwrap();
    assert!(!file_beside(
        &format!("{}/fake/bin/", root.display()),
        "trustc",
        None,
        None
    ));
    // `~` inside a path is a literal character, not home.
    let tilde = root.join("a~b/bin");
    std::fs::create_dir_all(&tilde).unwrap();
    std::fs::write(tilde.join("trustc"), b"").unwrap();
    assert!(file_beside(
        &format!("{}/", tilde.display()),
        "trustc",
        None,
        None
    ));
    let _ = std::fs::remove_dir_all(&root);
}

/// The shapes the interim script did not reach, each a way the shell runs a
/// command: prefix words with options, the compound-command keywords, a
/// flag cluster carrying `-c`, `eval`, `find -exec`, a leading redirection, a
/// quoted or escaped command word, a heredoc whose unquoted body substitutes.
#[test]
fn the_other_places_a_shell_runs_a_command_are_read() {
    for cmd in [
        "nice -n 5 cargo build",
        "timeout 60 cargo test",
        "timeout -s KILL 60 cargo test",
        "sudo -u me cargo build",
        "caffeinate -i cargo build --release",
        "xargs -I {} cargo build -p {}",
        "command cargo build",
        "exec cargo run",
        "nohup cargo test &",
        "! cargo test",
        "if cargo test; then echo ok; fi",
        "while cargo check; do sleep 1; done",
        "bash -lc 'cargo test'",
        "bash -o pipefail -c 'cargo test | tail'",
        "zsh -c \"rustfmt --check a.rs\"",
        "eval 'cargo build'",
        "find . -name '*.rs' -exec rustfmt {} \\;",
        "2>/dev/null cargo build",
        "> out.txt cargo build",
        "'cargo' build",
        "\\cargo build",
        "~/.cargo/bin/cargo build",
        "$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/cargo test",
        "cargo-clippy --all-targets",
        "cargo-fmt --all",
        "rustdoc src/lib.rs",
        "cat <<EOF\n$(cargo metadata)\nEOF",
        "echo a && cargo +stable build || true",
        "cargo trust check",
        "cargo --unverified build",
        "cargo --version",
    ] {
        assert!(check(cmd).is_some(), "MISSED {cmd:?}");
    }
}

/// Asking WHERE a tool is, or naming it as data, is never running it.
#[test]
fn asking_about_a_tool_and_naming_it_as_data_are_not_running_it() {
    for cmd in [
        "which cargo rustc",
        "command -v cargo",
        "command -V rustfmt",
        "type cargo",
        "rustup which cargo",
        "rustup toolchain list",
        "rustup show active-toolchain",
        "cat rust-toolchain.toml",
        "echo 'cargo build'",
        "printf '%s\\n' \"cargo clippy\"",
        "git grep -n 'cargo +1.97.1'",
        "ls $HOME/.cargo/bin",
        "cat <<'EOF'\n$(cargo metadata)\nEOF",
        "cat <<-EOF\n\tcargo test\n\tEOF",
        "echo \"<<EOF\" && targo tippy",
        "docker run --rm img cargo build",
        "ssh host cargo build",
        "CARGO_TARGET_DIR=t targo --unverified build",
        "RUSTUP_TOOLCHAIN=1.97.1 rustup show",
        "cargo_like_tool --help",
        "trustc -vV",
        "targo --unverified --version",
        "bash scripts/build.sh",
        "",
        "   ",
        "# cargo build",
    ] {
        assert_eq!(check(cmd), None, "FALSE POSITIVE {cmd:?}");
    }
}

/// The escape hatch is a non-empty assignment IN FRONT OF the one simple
/// command it clears. An empty reason, a different name, an `export` in an
/// earlier command, or the word anywhere else escapes nothing.
#[test]
fn the_escape_hatch_clears_exactly_one_simple_command() {
    assert_eq!(
        check("ATERM_STOCK_REASON='stock parity check' cargo +1.97.1 test"),
        None
    );
    assert_eq!(
        check("cd x && ATERM_STOCK_REASON=why env FOO=1 cargo test"),
        None
    );
    assert_eq!(check("ATERM_STOCK_REASON=why bash -c 'cargo test'"), None);
    for cmd in [
        "ATERM_STOCK_REASON= cargo test",
        "ATERM_STOCK_REASON='' cargo test",
        "ATERM_STOCK_REASONS=why cargo test",
        "export ATERM_STOCK_REASON=why; cargo test",
        "ATERM_STOCK_REASON=why true && cargo test",
        "cargo test ATERM_STOCK_REASON=why",
    ] {
        assert!(check(cmd).is_some(), "escaped by mistake: {cmd:?}");
    }
}

/// An escaped stock command is not dropped by [`verdict`]: it is reported with
/// the reason its author gave, so a guard can journal what ran and why. The
/// shell's own rule decides the value — the LAST assignment wins — and one
/// unescaped stock command anywhere on the line is still a refusal.
#[test]
fn an_escape_is_reported_with_its_reason_and_never_hides_a_second_command() {
    match verdict("ATERM_STOCK_REASON='measure stock parity' cargo +1.97.1 test -p x") {
        Verdict::Escaped { hit, reason } => {
            assert_eq!(reason, "measure stock parity");
            assert_eq!(hit.used, "cargo +1.97.1 test");
        }
        other => panic!("expected Escaped, got {other:?}"),
    }
    // Through `bash -c` and `find -exec`, the escape covers the one simple command.
    for cmd in [
        "ATERM_STOCK_REASON=why bash -c 'cargo test'",
        "ATERM_STOCK_REASON=why find . -name '*.rs' -exec rustfmt {} \\;",
    ] {
        assert!(
            matches!(verdict(cmd), Verdict::Escaped { ref reason, .. } if reason == "why"),
            "{cmd:?}: {:?}",
            verdict(cmd)
        );
    }
    // The last assignment wins, as in the shell: an empty one after a reason
    // leaves the command unescaped, a reason after an empty one escapes it.
    assert!(matches!(
        verdict("ATERM_STOCK_REASON=x ATERM_STOCK_REASON= cargo test"),
        Verdict::Stock(_)
    ));
    assert!(matches!(
        verdict("ATERM_STOCK_REASON= ATERM_STOCK_REASON=x cargo test"),
        Verdict::Escaped { .. }
    ));
    // An escaped command and an unescaped one on the same line: the refusal is
    // the unescaped one's, whichever comes first.
    for cmd in [
        "ATERM_STOCK_REASON=why cargo +1.97.1 test; cargo fmt --check",
        "cargo fmt --check; ATERM_STOCK_REASON=why cargo +1.97.1 test",
    ] {
        match verdict(cmd) {
            Verdict::Stock(hit) => assert_eq!(hit.used, "cargo fmt", "{cmd:?}"),
            other => panic!("{cmd:?}: expected Stock, got {other:?}"),
        }
    }
    // A substitution is ANOTHER command: the escape on `echo` does not reach it.
    assert!(matches!(
        verdict("ATERM_STOCK_REASON=why echo $(cargo metadata)"),
        Verdict::Stock(_)
    ));
    assert_eq!(verdict("targo --unverified test"), Verdict::Clean);
    // `check` is `verdict` with the escaped ones dropped.
    assert_eq!(check("ATERM_STOCK_REASON=why cargo test"), None);
}

/// The refusal a guard returns: the stock spelling as used, the Trust spelling
/// of the SAME command with the agent's own arguments, the fact that a stock
/// pin moves only rustup's proxies, the directory's own stock pin when it has
/// one, and the escape hatch — never an instruction to build with stock.
#[test]
fn the_refusal_names_the_trust_spelling_the_pin_and_the_escape() {
    let hit = check("cargo +1.97.1 test -p trust-cg-opt").expect("hit");
    let text = refusal(&hit, None);
    assert!(
        text.starts_with("`cargo +1.97.1 test` is stock Rust."),
        "{text}"
    );
    assert!(text.contains("moves only rustup's proxies"), "{text}");
    assert!(
        text.contains("never targo, tippy, trustfmt or trustdoc"),
        "{text}"
    );
    assert!(
        text.contains("\n  targo trust test -p trust-cg-opt "),
        "the verified lane with the caller's arguments: {text}"
    );
    assert!(
        text.contains("\n  targo --unverified test -p trust-cg-opt "),
        "{text}"
    );
    assert!(text.contains("ATERM_STOCK_REASON='<why>'"), "{text}");
    assert!(text.contains("aterm help rust"), "{text}");
    assert!(!text.contains("pins stock \""), "no pin was given: {text}");
    // A stock pin is named; a Trust pin is not a stock pin.
    let file = Path::new("/r/rust-toolchain.toml");
    let pinned = refusal(&hit, Some((file, "1.97.1")));
    assert!(
        pinned.contains("This directory pins stock \"1.97.1\" (/r/rust-toolchain.toml)"),
        "{pinned}"
    );
    assert_eq!(refusal(&hit, Some((file, "trust"))), text);
    // A verb targo is not known to have: the spelling is offered as CONDITIONAL,
    // never as the command to run (measured: `targo nextest` is "no such command").
    let ext = refusal(&check("cargo nextest run -p x").expect("hit"), None);
    assert!(ext.contains("not one targo is known to have"), "{ext}");
    assert!(
        ext.contains("IF targo has it, the Trust spelling is:\n  targo nextest run -p x\n"),
        "{ext}"
    );
    assert!(ext.contains("no Trust spelling yet"), "{ext}");
    assert!(
        !ext.contains("Run the Trust spelling of the same command"),
        "{ext}"
    );
    assert!(!text.contains("not one targo is known to have"), "{text}");
    // One spelling, no lane: the linter.
    let lint = refusal(
        &check("cargo clippy --all-targets -- -D warnings").expect("hit"),
        None,
    );
    assert!(
        lint.contains("\n  targo tippy --all-targets -- -D warnings\n"),
        "{lint}"
    );
    assert!(!lint.contains("targo trust"), "{lint}");
    // Never an instruction to use stock, and never "the project wins": only the
    // first line quotes the stock spelling, and every command offered is a
    // Trust tool with no rustup directive.
    for t in [&text, &pinned, &lint] {
        assert!(!t.contains("wins"), "{t}");
        let offered: Vec<&str> = t.lines().filter(|l| l.starts_with("  ")).collect();
        assert!(!offered.is_empty(), "{t}");
        for line in offered {
            assert!(line.trim_start().starts_with("targo "), "{line:?} in {t}");
            assert!(!line.contains('+'), "no rustup directive: {line:?}");
        }
    }
}

/// The refusal is bounded: a kilobyte argument does not become a kilobyte
/// line, a quoted newline does not become a line break, and the cut never
/// splits a character.
#[test]
fn the_refusal_is_bounded_and_one_line_per_command() {
    let long = format!("cargo test -- {}", "é".repeat(2_000));
    let text = refusal(&check(&long).expect("hit"), None);
    for line in text.lines() {
        assert!(
            line.len() <= REFUSAL_COMMAND_CAP + 64,
            "{} bytes: {line:?}",
            line.len()
        );
    }
    assert!(text.contains('…'), "{text}");
    let text = refusal(&check("cargo test -- 'a\nb'").expect("hit"), None);
    assert!(text.contains("targo trust test -- a b "), "{text}");
}

/// What the hit SAYS: the stock spelling as used, and the Trust spelling of the
/// same command with the caller's own arguments — the lane that works, not a
/// generic hint. The incident's own command reads back as its two targo lanes.
#[test]
fn a_hit_names_what_was_used_and_the_trust_command_with_its_arguments() {
    let hit = check("cargo +1.97.1 test -p trust-cg-opt").expect("hit");
    assert_eq!(hit.used, "cargo +1.97.1 test");
    assert_eq!(
        commands(&hit.instead),
        [
            "targo trust test -p trust-cg-opt",
            "targo --unverified test -p trust-cg-opt"
        ]
    );
    let hit = check("CARGO_BUILD_JOBS=4 cargo clippy --all-targets -- -D warnings").expect("hit");
    assert_eq!(hit.used, "cargo clippy");
    assert_eq!(
        commands(&hit.instead),
        ["targo tippy --all-targets -- -D warnings"]
    );
    let hit = check("rustup run 1.97.1 rustfmt --check a.rs").expect("hit");
    assert_eq!(hit.used, "rustup run 1.97.1 rustfmt");
    assert_eq!(commands(&hit.instead), ["trustfmt --check a.rs"]);
    let hit = check("rustfmt +1.97.1 x.rs").expect("hit");
    assert_eq!(hit.used, "rustfmt +1.97.1");
    assert_eq!(commands(&hit.instead), ["trustfmt x.rs"]);
    // A redirection belongs to the shell: it is neither the verb nor an
    // argument of the Trust command (the corpus had `cargo 2>` as a "verb").
    let hit = check("timeout 60 cargo --version 2>&1 | head -3").expect("hit");
    assert_eq!(hit.used, "cargo");
    assert_eq!(commands(&hit.instead), ["targo --version"]);
    let hit = check("cargo +1.97.1 test -p x 2>&1 >/tmp/log").expect("hit");
    assert_eq!(hit.used, "cargo +1.97.1 test");
    assert_eq!(
        commands(&hit.instead),
        ["targo trust test -p x", "targo --unverified test -p x"]
    );
    let hit = check("rustc +stable main.rs").expect("hit");
    assert_eq!(hit.used, "rustc +stable");
    assert_eq!(
        commands(&hit.instead),
        ["trustc main.rs", "trustc -Ztrust-verify=off main.rs"]
    );
}

/// Every cargo verb renders the spelling targo ACCEPTS for it — measured on
/// store build 9192 (the module table). The lane flag on a verb that refuses it
/// is the HOST-05 shape: a hint the reader cannot run.
#[test]
fn every_cargo_verb_gets_the_lane_targo_accepts_for_it() {
    let spell =
        |a: &[&str]| commands(&trust_spelling("cargo", &args(a)).expect("cargo")).join(" | ");
    assert_eq!(
        spell(&["build", "--release"]),
        "targo trust build --release | targo --unverified build --release"
    );
    assert_eq!(spell(&["run", "-p", "x"]), "targo --unverified run -p x");
    assert_eq!(
        spell(&["doc", "--no-deps"]),
        "targo --unverified doc --no-deps"
    );
    assert_eq!(
        spell(&["install", "--path", "."]),
        "targo --unverified install --path ."
    );
    assert_eq!(
        spell(&["metadata", "--format-version", "1"]),
        "targo metadata --format-version 1"
    );
    assert_eq!(spell(&["fmt", "--check"]), "targo fmt --check");
    assert_eq!(spell(&["tree"]), "targo tree");
    assert_eq!(spell(&["clippy", "-p", "x"]), "targo tippy -p x");
    assert_eq!(spell(&["trust", "check"]), "targo trust check");
    assert_eq!(
        spell(&["--unverified", "build"]),
        "targo --unverified build"
    );
    assert_eq!(spell(&["--version"]), "targo --version");
    assert_eq!(spell(&[]), "targo");
    // A leading `+<toolchain>` is dropped: no Trust tool takes a rustup directive.
    assert_eq!(spell(&["+1.97.1", "fmt"]), "targo fmt");
    assert_eq!(
        spell(&["+trust", "check"]),
        "targo trust check | targo --unverified check"
    );
    // A global flag before the verb moves behind it: `targo trust` reads its
    // verb first.
    assert_eq!(
        spell(&["-v", "--config", "net.offline=true", "check"]),
        "targo trust check -v --config net.offline=true | targo --unverified check -v --config net.offline=true"
    );
    // An unknown verb is rendered, with the note that says it may not exist.
    let unknown = trust_spelling("cargo", &args(&["nextest", "run"])).expect("cargo");
    assert_eq!(commands(&unknown), ["targo nextest run"]);
    assert!(unknown[0].note.contains("cargo extension"), "{unknown:?}");
    // The classes are disjoint: no verb in two rows.
    for verb in BOTH_LANES.iter().chain(UNVERIFIED_ONLY).chain(NO_LANE) {
        let rows = [BOTH_LANES, UNVERIFIED_ONLY, NO_LANE]
            .iter()
            .filter(|row| row.contains(verb))
            .count();
        assert_eq!(rows, 1, "{verb} is in {rows} rows");
    }
    assert_eq!(trust_spelling("targo", &args(&["build"])), None);
}

#[test]
fn spellings_render_as_one_padded_column() {
    let two = trust_spelling("cargo", &args(&["test", "-p", "x"])).expect("cargo");
    assert_eq!(
        render_spellings(&two, "  "),
        "  targo trust test -p x          VERIFIED   — emits a proof claim\n  \
         targo --unverified test -p x   UNVERIFIED — no proof claim\n"
    );
    let one = trust_spelling("rustfmt", &args(&["a.rs"])).expect("rustfmt");
    assert_eq!(render_spellings(&one, "    "), "    trustfmt a.rs\n");
}

#[test]
fn a_pin_is_read_the_way_rustup_reads_it() {
    assert_eq!(
        channel_in("[toolchain]\nchannel = \"1.97.1\"\ncomponents = [\"rustfmt\", \"clippy\"]\n")
            .as_deref(),
        Some("1.97.1")
    );
    assert_eq!(
        channel_in("# THE toolchain\n[toolchain]\nchannel = \"trust\"\n").as_deref(),
        Some("trust")
    );
    assert_eq!(channel_in("channel='nightly'").as_deref(), Some("nightly"));
    // The legacy one-line form.
    assert_eq!(
        channel_in("nightly-2026-03-12\n").as_deref(),
        Some("nightly-2026-03-12")
    );
    // No channel named.
    assert_eq!(channel_in("[toolchain]\npath = \"/x\"\n"), None);
    assert_eq!(channel_in("channel_x = \"y\"\n[toolchain]\n"), None);
    assert_eq!(channel_in(""), None);

    let root = std::env::temp_dir().join(format!("aterm-rust-lane-pin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let deep = root.join("crates").join("x").join("src");
    std::fs::create_dir_all(&deep).expect("mkdir");
    std::fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.97.1\"\n",
    )
    .expect("write");
    let (file, ch) = find_pin(&deep).expect("found walking up");
    assert_eq!(ch, "1.97.1");
    assert_eq!(file, root.join("rust-toolchain.toml"));
    // `rust-toolchain` wins over `rust-toolchain.toml` in one directory.
    std::fs::write(root.join("rust-toolchain"), "stable\n").expect("write");
    assert_eq!(find_pin(&deep).map(|(_, ch)| ch).as_deref(), Some("stable"));
    // A nearer file wins.
    std::fs::write(
        root.join("crates").join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"trust\"\n",
    )
    .expect("write");
    assert_eq!(find_pin(&deep).map(|(_, ch)| ch).as_deref(), Some("trust"));
    assert!(is_trust_channel("trust"));
    assert!(is_trust_channel("trust-9192"));
    assert!(!is_trust_channel("1.97.1"));
    let _ = std::fs::remove_dir_all(&root);
}

/// Pathological input is bounded, never a hang or a panic.
/// NESTING PAST THE CAP IS REFUSED, NOT WAVED THROUGH (review of 2026-09-27):
/// nine or more nested `$( )`, `bash -c` or `eval` levels around `cargo build`
/// answered `Clean`, and this test pinned that as the design — a guard's own way
/// past itself. Text past [`MAX_DEPTH`] is still not parsed, but a stock tool's
/// NAME in it is a hit marked [`Hit::unread`]; the escape in front of the command
/// that runs it (`bash -c`, `eval`) clears it as it clears any hit inside, and
/// deep text naming no stock tool stays clean. The cost, pinned too: past the cap
/// a stock name is refused even where it is only data.
#[test]
fn nesting_past_the_cap_is_refused_and_malformed_input_is_bounded() {
    let nest = |kind: &str, inner: &str, levels: usize| -> String {
        let mut s = inner.to_string();
        for _ in 0..levels {
            s = match kind {
                "subst" => format!("echo $( {s} )"),
                "bash" => format!("bash -c {}", shell_quote(&s)),
                "eval" => format!("eval {s}"),
                _ => unreachable!(),
            };
        }
        s
    };
    for kind in ["subst", "bash", "eval"] {
        // 12 levels, past the bound of 8 (`bash -c` quoting grows with each level,
        // so the count stays small on purpose).
        for levels in 1..=12 {
            let cmd = nest(kind, "cargo build", levels);
            let hit = check(&cmd).unwrap_or_else(|| panic!("MISSED {kind} x{levels}"));
            let past_the_cap = levels > MAX_DEPTH;
            assert_eq!(hit.unread, past_the_cap, "{kind} x{levels}");
            assert_eq!(
                hit.used,
                if past_the_cap { "cargo" } else { "cargo build" },
                "{kind} x{levels}"
            );
            if kind != "subst" {
                // A substitution in an argument is the shell's, outside the
                // escaped command; `bash -c` and `eval` run theirs inside it.
                let escaped = format!("ATERM_STOCK_REASON='deep probe' {cmd}");
                assert!(
                    matches!(verdict(&escaped), Verdict::Escaped { .. }),
                    "{kind} x{levels}"
                );
            }
        }
        for clean in ["echo hi", "targo --unverified build", "trustc -vV"] {
            assert_eq!(check(&nest(kind, clean, 12)), None, "{kind}: {clean:?}");
        }
        assert!(
            check(&nest(kind, "echo cargo", 12)).is_some_and(|h| h.unread),
            "{kind}: past the cap a stock name is refused even as data"
        );
    }
    let hit = check(&nest("bash", "cargo build", 12)).expect("refused");
    let text = refusal(&hit, Some((Path::new("/r/rust-toolchain.toml"), "1.97.1")));
    assert!(
        text.starts_with("`cargo` appears in text nested more than 8 levels deep"),
        "{text}"
    );
    assert!(text.contains("flatten the command"), "{text}");
    assert!(text.contains("The Trust tool for it:\n  targo\n"), "{text}");
    assert!(text.contains("ATERM_STOCK_REASON='<why>'"), "{text}");
    assert!(!text.contains("is stock Rust"), "{text}");
    assert!(!text.contains("pins stock"), "{text}");
    for cmd in [
        "echo 'unterminated",
        "echo \"unterminated",
        "$(",
        "`",
        "<<",
        "cat <<",
        "cat <<'",
        "\\",
        "((((",
        "))))",
    ] {
        let _ = check(cmd);
    }
    assert_eq!(check("echo 'unterminated cargo build"), None);
}

/// A TRUST TOOLCHAIN CHOSEN BY ITS RUSTUP CHANNEL (review of 2026-09-27): `rustup
/// run trust <tool>` is the spelling `aterm help reroute` sanctions, and a Trust
/// channel's `rustc`/`rustdoc` are that toolchain's trustc/trustdoc — so `rustup
/// run trust rustc` and `rustc +trust` are not stock, as `<dir>/rustc` beside
/// `<dir>/trustc` is not. Its `cargo` stays a hit ([`COMPAT_TWINS`]); a stock
/// channel's tools, a `rustfmt` and a bare name after it stay hits.
#[test]
fn a_trust_channels_own_rustc_and_rustdoc_are_not_stock() {
    for cmd in [
        "rustup run trust rustc -vV",
        "rustup run trust-45040366 rustdoc --version",
        "rustc +trust -vV",
        "rustdoc +trust --version",
        "x=$(rustup run trust rustc --print sysroot)",
        "timeout 10 rustup run trust rustc -vV 2>&1 | head -3",
    ] {
        assert_eq!(verdict(cmd), Verdict::Clean, "{cmd:?}");
    }
    for cmd in [
        "rustup run trust cargo build",
        "cargo +trust build",
        "rustup run trust rustfmt a.rs",
        "rustup run 1.97.1 rustc -vV",
        "rustc +1.97.1 -vV",
        "rustc +stable a.rs",
        "rustup run stable rustdoc --version",
        "rustup run trust rustc -vV && rustc -vV",
    ] {
        assert!(check(cmd).is_some(), "MISSED {cmd:?}");
    }
}

fn shell_quote(s: &str) -> String {
    let mut q = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            q.push_str("'\\''");
        } else {
            q.push(c);
        }
    }
    q.push('\'');
    q
}

/// THE REVIEWER'S PROBES (2026-09-24, review of tc-engines-a): the interim
/// `~/.claude/hooks/trust-toolchain-guard.py` passes its own (then 33) cases and let no
/// allow probe through, but MISSED 13 command shapes — the compound-command
/// keywords (`if`, `if !`, `while`, `until`), `timeout`, `xargs`, `find -exec`,
/// wrappers that take options (`nice -n`, `env -u`, `caffeinate -i`, `stdbuf
/// -oL`, `ionice`) and a leading redirection. Each is a way the shell runs the
/// tool, so each must be read here — beside the allow shapes the same review
/// probed: heredoc bodies, quoted strings, grep/sed patterns, `./x.py`,
/// `$(rustup which rustc)`, `rustup run trust targo`, make targets.
#[test]
fn the_reviewers_thirteen_missed_shapes_are_read_and_the_allow_probes_still_pass() {
    let missed_by_the_interim_guard = [
        "if cargo build; then echo ok; fi",
        "if ! cargo test; then echo broken; fi",
        "while cargo check; do sleep 5; done",
        "until cargo build; do sleep 5; done",
        "timeout 60 cargo build",
        "ls crates | xargs cargo build -p",
        "find . -name Cargo.toml -exec cargo build --manifest-path {} \\;",
        "nice -n 19 cargo build --release",
        "env -u RUSTFLAGS cargo build",
        "caffeinate -i cargo test",
        "stdbuf -oL cargo test 2>&1 | tee log",
        "ionice cargo build",
        "2>&1 cargo build",
    ];
    assert_eq!(missed_by_the_interim_guard.len(), 13);
    for cmd in missed_by_the_interim_guard {
        assert!(check(cmd).is_some(), "MISSED {cmd:?}");
    }
    // The same wrappers in their other spellings.
    for cmd in [
        "elif cargo test; then :; fi",
        "if ! timeout 900 cargo test; then exit 1; fi",
        "stdbuf -o L -e L cargo test",
        "stdbuf --output=L cargo build",
        "ionice -c 3 cargo build",
        "ionice -c3 -n7 rustc a.rs",
        "nice -n19 cargo build",
        "env -i PATH=/usr/bin cargo build",
        "/usr/bin/time -p cargo tree",
        "/usr/bin/time -f %e cargo build",
        "time -o t.txt cargo build",
        "taskset -c 0-3 cargo build",
        "taskset 0x3 cargo build",
        "xargs -0 -n1 cargo fmt --",
        "find . -execdir rustfmt {} +",
        "</dev/null cargo build",
        "&>log cargo build",
    ] {
        assert!(check(cmd).is_some(), "MISSED {cmd:?}");
    }
    for cmd in [
        "cat <<EOF\ncargo build --release\nEOF",
        "cat <<'EOF'\nrustfmt a.rs\nEOF",
        "echo \"cargo clippy\"",
        "echo 'rustc --version'",
        "grep -rn 'cargo +1.97.1' scripts",
        "sed -n '/cargo build/p' log.txt",
        "sed -i 's/rustfmt/trustfmt/g' a.sh",
        "./x.py build --stage 1",
        "\"$(rustup which rustc)\" --print sysroot",
        "rustup run trust targo --unverified build",
        "make -C crates build",
        "make test",
        "git log --grep 'cargo test'",
        "ionice -c3 targo --unverified build",
        "stdbuf -oL targo --unverified test",
        "timeout 60 targo trust check",
        "if targo --unverified check; then echo ok; fi",
    ] {
        assert_eq!(check(cmd), None, "FALSE POSITIVE {cmd:?}");
    }
}

/// THE tc-aterm REVIEW'S THREE FALSE POSITIVES AND ITS HEREDOC MISS
/// (2026-09-24), each measured against the interim script too — which gave the
/// same verdicts, so they were inherited, not introduced:
/// * `${DIR}/cargo` — the reader split simple commands on `{`/`}` anywhere, so
///   the parameter expansion yielded a segment `/cargo`;
/// * a multi-line `case` arm `cargo)` — the `)` closing a case PATTERN was read
///   as a subshell's end, and the pattern as a command;
/// * `<<'EOF-1'` — a delimiter was read as `[A-Za-z0-9_]+`, so `EOF` never
///   matched `EOF-1` and every later line was swallowed as heredoc body.
#[test]
fn a_parameter_expansion_a_case_pattern_and_a_dashed_heredoc_delimiter_are_read_as_the_shell_reads_them()
 {
    for cmd in [
        "ls ${DIR}/cargo",
        "echo ${SYSROOT}/bin/rustc",
        "test -x ${CARGO_HOME:-$HOME/.cargo}/bin/cargo && echo yes",
        "case \"$x\" in\n  cargo) echo c;;\n  rustc|rustfmt) echo r;;\nesac",
        "case $tool in\n(cargo) echo c ;;\n*) echo other ;;\nesac",
        "case \"$x\" in cargo) echo c;; esac",
    ] {
        assert_eq!(check(cmd), None, "FALSE POSITIVE {cmd:?}");
    }
    for cmd in [
        // A command in a case ARM runs, as it does anywhere else.
        "case \"$x\" in\n  build) cargo build;;\nesac",
        "case \"$x\" in build) cargo build;; esac",
        "case $x in\n  a)\n    cargo test\n    ;;\nesac",
        // After the case, commands are commands again.
        "case $x in\n  cargo) echo c;;\nesac\ncargo build",
        // A dashed or dotted delimiter ends where it says.
        "cat <<'EOF-1'\nnot a command\nEOF-1\ncargo build",
        "cat <<END.txt\nbody\nEND.txt\nrustfmt a.rs",
        // A parameter expansion as the command word is still read by its value's
        // shape when it is a literal path.
        "${HOME}/.cargo/bin/cargo build",
        // A substitution inside an expansion still runs.
        "echo ${X:-$(cargo metadata --format-version 1)}",
    ] {
        assert!(check(cmd).is_some(), "MISSED {cmd:?}");
    }
}
