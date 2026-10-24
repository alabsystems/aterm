// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify provision` on a fake host in a scratch HOME.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::*;

enum Member<'a> {
    Dir,
    File(&'a [u8], u32),
    Symlink(&'a str),
}

fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(text.as_bytes());
    field[digits] = 0;
}

fn tar(members: &[(&str, Member<'_>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, member) in members {
        let (kind, mode, body): (u8, u32, &[u8]) = match member {
            Member::Dir => (b'5', 0o755, &[]),
            Member::File(body, mode) => (b'0', *mode, body),
            Member::Symlink(_) => (b'2', 0o777, &[]),
        };
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        if let Member::Symlink(target) = member {
            h[157..157 + target.len()].copy_from_slice(target.as_bytes());
        }
        octal(&mut h[100..108], u64::from(mode));
        octal(&mut h[108..116], 0);
        octal(&mut h[116..124], 0);
        octal(&mut h[124..136], body.len() as u64);
        octal(&mut h[136..148], 1_790_000_000);
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    let mut rest = data;
    loop {
        let (block, tail) = rest.split_at(rest.len().min(0xFFFF));
        out.push(u8::from(tail.is_empty()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
        if tail.is_empty() {
            break;
        }
        rest = tail;
    }
    out.extend_from_slice(&aterm_codec::crc32::Crc32::of(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

fn macho(dylibs: &[&str], tail: &[u8]) -> Vec<u8> {
    let mut commands = Vec::new();
    for name in dylibs {
        let size = (24 + name.len() + 1).div_ceil(8) * 8;
        for w in [0xCu32, size as u32, 24, 2, 0x1_0000, 0x1_0000] {
            commands.extend_from_slice(&w.to_le_bytes());
        }
        let start = commands.len();
        commands.extend_from_slice(name.as_bytes());
        commands.resize(start + size - 24, 0);
    }
    let mut m = 0xCFFA_EDFEu32.to_be_bytes().to_vec();
    for w in [
        0x0100_000C,
        0,
        2,
        dylibs.len() as u32,
        commands.len() as u32,
        0,
        0,
    ] {
        m.extend_from_slice(&w.to_le_bytes());
    }
    m.extend_from_slice(&commands);
    m.extend_from_slice(tail);
    m
}

const LIBSYSTEM: &str = "/usr/lib/libSystem.B.dylib";
const HOMEBREW_PCRE2: &str = "/opt/homebrew/lib/libpcre2-32.dylib";
const OTHER_TEAM: &[u8] = b"TEAM=OTHERTEAM1";

fn sha256(bytes: &[u8]) -> String {
    let mut h = aterm_digest::Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn fixture_row(
    name: &'static str,
    tool: &'static str,
    url: &'static str,
    sha256: &str,
    bytes: u64,
    kind: impl FnOnce(&'static [Archive]) -> Install,
) -> Row {
    let archives: &'static [Archive] = Box::leak(Box::new([Archive {
        platform: "macos-aarch64",
        url,
        sha256: leak(sha256.to_string()),
        bytes,
    }]));
    Row {
        name,
        tool: Some(tool),
        install: kind(archives),
    }
}

struct Provisioned {
    pin: String,
}

impl stable::Rustup for Provisioned {
    fn run(
        &self,
        _homes: &stable::Homes,
        _tool: &Path,
        args: &[String],
        toolchain: Option<&str>,
    ) -> Result<String, String> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let host = "aarch64-apple-darwin";
        Ok(match (a.as_slice(), toolchain) {
            (["toolchain", "list"], _) => format!("stable-{host}\n{}-{host}\n", self.pin),
            (["target", "list", "--installed"], Some("stable")) => {
                let mut t: Vec<&str> = FOREIGN_CELL_TARGETS.to_vec();
                t.push("x86_64-apple-darwin");
                t.join("\n")
            }
            (["target", "list", "--installed"], _) => "wasm32-unknown-unknown\n".into(),
            (["component", "list", "--installed"], _) => format!("clippy-{host}\n"),
            (other, _) => return Err(format!("this rustup changes nothing: {other:?}")),
        })
    }
}

#[derive(Default)]
struct Bare {
    calls: RefCell<Vec<(PathBuf, bool, String)>>,
}

impl stable::Rustup for Bare {
    fn run(
        &self,
        homes: &stable::Homes,
        tool: &Path,
        args: &[String],
        _toolchain: Option<&str>,
    ) -> Result<String, String> {
        self.calls.borrow_mut().push((
            tool.to_path_buf(),
            place::is_executable(tool),
            args.join(" "),
        ));
        if tool.file_name().is_some_and(|n| n == "rustup-init") {
            let bin = homes.cargo_home.join("bin/rustup");
            std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
            std::fs::write(&bin, "#!/bin/false\n").unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Ok(String::new())
    }
}

struct Fake {
    served: BTreeMap<String, Vec<u8>>,
    refused: BTreeMap<String, String>,
    fetched: RefCell<Vec<String>>,
    fetched_into: RefCell<Vec<(PathBuf, u64)>>,
    spawns: RefCell<Vec<Spawn>>,
    signed: RefCell<Vec<PathBuf>>,
    built_fish: Vec<u8>,
    fish_answers: String,
    fish_error: Option<&'static str>,
    fish_panics: bool,
    built_bash: Vec<u8>,
    bash_answers: String,
    patched: RefCell<Vec<u8>>,
    /// The `y.tab.c` the fake `make bash` compiled.
    parser: RefCell<Vec<u8>>,
    bindgen_answers: String,
    zip_files: Vec<(&'static str, Vec<u8>)>,
    zip_links: Vec<(&'static str, PathBuf)>,
    rustup: Provisioned,
    bare: Option<Bare>,
}

impl Fake {
    fn new() -> Self {
        Self {
            served: BTreeMap::new(),
            refused: BTreeMap::new(),
            fetched: RefCell::default(),
            spawns: RefCell::default(),
            signed: RefCell::default(),
            fetched_into: RefCell::default(),
            built_fish: macho(&[LIBSYSTEM], b""),
            fish_answers: "fish, version 4.9.3\n".into(),
            fish_error: None,
            fish_panics: false,
            built_bash: macho(&[LIBSYSTEM, "/usr/lib/libncurses.5.4.dylib"], b""),
            bash_answers: "GNU bash, version 5.3.20(1)-release (aarch64-apple-darwin27.0.0)\n\
                           Copyright (C) 2025 Free Software Foundation, Inc.\n"
                .into(),
            patched: RefCell::default(),
            parser: RefCell::default(),
            bindgen_answers: format!("wasm-bindgen {}\n", crate::wasm_glue::BINDGEN),
            zip_files: vec![(
                "chrome-headless-shell-mac-arm64/chrome-headless-shell",
                b"#!/bin/false\n".to_vec(),
            )],
            zip_links: Vec::new(),
            rustup: Provisioned {
                pin: stable::wasm_pin(&repo()).expect("the tree's wasm32 pin"),
            },
            bare: None,
        }
    }

    fn serve(mut self, url: &str, bytes: Vec<u8>) -> Self {
        self.served.insert(url.to_string(), bytes);
        self
    }

    /// Fail the fetch of `url` with `error`, as `System::run` words curl's failure.
    fn refuse(mut self, url: &str, error: &str) -> Self {
        self.refused.insert(url.to_string(), error.to_string());
        self
    }

    fn spawned(&self, program_ends: &str) -> Vec<Spawn> {
        self.spawns
            .borrow()
            .iter()
            .filter(|s| s.program.ends_with(program_ends))
            .cloned()
            .collect()
    }
}

fn arg_after(spawn: &Spawn, flag: &str) -> PathBuf {
    let i = spawn.args.iter().position(|a| a == flag).expect(flag);
    PathBuf::from(&spawn.args[i + 1])
}

impl Host for Fake {
    fn fetch(&self, url: &str, dest: &Path, bytes: u64) -> Result<(), String> {
        self.fetched.borrow_mut().push(url.to_string());
        self.fetched_into
            .borrow_mut()
            .push((dest.to_path_buf(), bytes));
        if let Some(error) = self.refused.get(url) {
            return Err(error.clone());
        }
        let bytes = self.served.get(url).ok_or_else(|| format!("404 {url}"))?;
        assert!(
            std::fs::read_dir(dest.parent().unwrap())
                .unwrap()
                .next()
                .is_none(),
            "each download lands in an empty directory of its own"
        );
        std::fs::write(dest, bytes).map_err(|e| e.to_string())
    }

    fn developer_id(&self, path: &Path, team: &str) -> Result<(), String> {
        assert_eq!(team, MICROSOFT_TEAM);
        self.signed.borrow_mut().push(path.to_path_buf());
        let bytes = std::fs::read(path).unwrap();
        if bytes.windows(OTHER_TEAM.len()).any(|w| w == OTHER_TEAM) {
            Err("signed by team OTHERTEAM1".into())
        } else {
            Ok(())
        }
    }

    fn run(&self, spawn: &Spawn) -> Result<String, String> {
        self.spawns.borrow_mut().push(spawn.clone());
        let args: Vec<String> = spawn
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let program = spawn.program.to_string_lossy();
        match a.as_slice() {
            ["-c", "-f", out, from] if program == place::TAR => {
                std::fs::copy(from.trim_start_matches('@'), out).map_err(|e| e.to_string())?;
                Ok(String::new())
            }
            ["--unverified", "build", ..] => {
                let fish = arg_after(spawn, "--target-dir").join("release/fish");
                std::fs::create_dir_all(fish.parent().unwrap()).unwrap();
                std::fs::write(&fish, &self.built_fish).unwrap();
                Ok(String::new())
            }
            ["--unverified", "install", "wasm-bindgen-cli", ..] => {
                let root = arg_after(spawn, "--root");
                std::fs::create_dir_all(root.join("bin")).unwrap();
                std::fs::write(root.join("bin/wasm-bindgen"), "#!/bin/false\n").unwrap();
                Ok(String::new())
            }
            ["-p0", "-s", "-f", "-i", patch] if program == bash::PATCH => {
                let body = std::fs::read(patch).map_err(|e| e.to_string())?;
                if body.windows(6).any(|w| w == b"REJECT") {
                    return Err(format!("{program} exited Some(1): 1 out of 1 hunks failed"));
                }
                self.patched.borrow_mut().extend_from_slice(&body);
                // A `+++ <file>` line changes that file, in the patch's order.
                for line in String::from_utf8_lossy(&body).lines() {
                    if let Some(file) = line.strip_prefix("+++ ") {
                        std::fs::write(spawn.cwd.join(file), &body).map_err(|e| e.to_string())?;
                    }
                }
                Ok(String::new())
            }
            ["./configure", ..] if program == bash::SH => {
                let time = |name: &str| {
                    std::fs::metadata(spawn.cwd.join(name))
                        .and_then(|m| m.modified())
                        .unwrap()
                };
                if ["configure.ac", "aclocal.m4"]
                    .iter()
                    .any(|from| time("configure") < time(from))
                {
                    return Err(
                        "make would remake configure with autoconf, which no Mac has".into(),
                    );
                }
                std::fs::write(spawn.cwd.join("Makefile"), "bash:\n").map_err(|e| e.to_string())?;
                Ok(String::new())
            }
            ["bash"] if program == bash::MAKE => {
                assert!(spawn.cwd.join("Makefile").is_file(), "configure ran first");
                // `y.tab.c: parse.y`, as the Command Line Tools' make 3.81 judges it: by
                // whole seconds, a `parse.y` newer than `y.tab.c` is run through bison.
                let (parser, grammar) = (spawn.cwd.join("y.tab.c"), spawn.cwd.join("parse.y"));
                let secs = |file: &Path| {
                    std::fs::metadata(file)
                        .and_then(|m| m.modified())
                        .unwrap()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                };
                if secs(&grammar) > secs(&parser) {
                    std::fs::copy(&grammar, &parser).unwrap();
                }
                *self.parser.borrow_mut() = std::fs::read(&parser).unwrap();
                std::fs::write(spawn.cwd.join("bash"), &self.built_bash).unwrap();
                Ok(String::new())
            }
            ["--version"] if program.ends_with("bin/bash") => Ok(self.bash_answers.clone()),
            ["--version"] if program.ends_with("bin/fish") => {
                assert!(!self.fish_panics, "the injected version probe unwinds");
                if let Some(e) = self.fish_error {
                    Err(e.into())
                } else {
                    Ok(self.fish_answers.clone())
                }
            }
            ["--version"] if program.ends_with("wasm-bindgen") => {
                if program.contains(place::SCRATCH_PREFIX) {
                    Ok(format!("wasm-bindgen {}\n", crate::wasm_glue::BINDGEN))
                } else {
                    Ok(self.bindgen_answers.clone())
                }
            }
            ["-x", "-k", _, tree] if program == "/usr/bin/ditto" => {
                use std::os::unix::fs::PermissionsExt as _;
                for (name, bytes) in &self.zip_files {
                    let file = Path::new(tree).join(name);
                    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                    std::fs::write(&file, bytes).unwrap();
                    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
                for (name, to) in &self.zip_links {
                    let link = Path::new(tree).join(name);
                    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
                    std::os::unix::fs::symlink(to, link).unwrap();
                }
                Ok(String::new())
            }
            other => Err(format!("the fake runs no {program} {other:?}")),
        }
    }

    fn targo(&self) -> Result<PathBuf, String> {
        Ok(PathBuf::from("/stage2/bin/targo"))
    }

    fn rustup(&self) -> &dyn stable::Rustup {
        match &self.bare {
            Some(bare) => bare,
            None => &self.rustup,
        }
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn machine(home: &Path) -> Machine {
    let rustup = home.join(".cargo/bin/rustup");
    std::fs::create_dir_all(rustup.parent().unwrap()).unwrap();
    std::fs::write(&rustup, "#!/bin/false\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    Machine {
        platform: "macos-aarch64".into(),
        tooling: home.join("Library/Caches/aterm/tooling.noindex"),
        browser: home.join("Library/Caches/aterm/browser.noindex"),
        repo: repo(),
        homes: stable::Homes {
            home: home.to_path_buf(),
            cargo_home: home.join(".cargo"),
            rustup_home: home.join(".rustup"),
            path: None,
            homebrew_keg: None,
        },
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

const BINARYEN_URL: &str = "https://fixture.invalid/binaryen-version_{v}-arm64-macos.tar.gz";

fn binaryen_archive() -> Vec<u8> {
    let wasm_opt = macho(&["@rpath/libbinaryen.dylib"], b"");
    gzip(&tar(&[
        ("binaryen-version_133/", Member::Dir),
        ("binaryen-version_133/bin/", Member::Dir),
        (
            "binaryen-version_133/bin/wasm-opt",
            Member::File(&wasm_opt, 0o755),
        ),
        ("binaryen-version_133/lib/", Member::Dir),
        (
            "binaryen-version_133/lib/libbinaryen.dylib",
            Member::File(b"lib", 0o755),
        ),
    ]))
}

fn binaryen_row(sha256: &str, bytes: u64) -> Row {
    fixture_row(
        "binaryen",
        "wasm-opt",
        BINARYEN_URL,
        sha256,
        bytes,
        |archives| Install::Unpack {
            archives,
            strip: 1,
            anchor: Anchor::Sha256,
            keep: &[],
            cap: None,
            holds: Holds::Program,
        },
    )
}

#[test]
fn a_wrong_sha256_refuses_the_row_and_leaves_nothing() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let archive = binaryen_archive();
    let url = BINARYEN_URL.replace("{v}", "133");
    let host = Fake::new().serve(&url, archive.clone());
    let len = archive.len() as u64;

    let wrong = binaryen_row(&sha256(b"another archive"), len);
    let state = provide(&wrong, &m, &host, &mut |_| {});
    match &state {
        State::Failed(why) => assert!(
            why.contains("hashes to") && why.contains("nothing was unpacked"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        names(&m.tooling),
        Vec::<String>::new(),
        "nothing left behind"
    );

    let short = binaryen_row(&sha256(&archive), len + 1);
    assert!(
        matches!(provide(&short, &m, &host, &mut |_| {}), State::Failed(w) if w.contains("bytes"))
    );
    assert_eq!(names(&m.tooling), Vec::<String>::new());

    let right = binaryen_row(&sha256(&archive), len);
    let exe = m.tooling.join("binaryen-133/bin/wasm-opt");
    assert_eq!(
        provide(&right, &m, &host, &mut |_| {}),
        State::Placed(exe.clone())
    );
    assert_eq!(
        names(&m.tooling),
        ["binaryen-133"],
        "the scratch went with the run"
    );
    assert_eq!(mode(&exe), 0o755);
    assert!(
        m.tooling
            .join("binaryen-133/lib/libbinaryen.dylib")
            .is_file()
    );
    let stamp = place::read_stamp(&m.tooling.join("binaryen-133")).unwrap();
    assert_eq!(stamp.sha256.as_deref(), Some(sha256(&archive).as_str()));
    assert_eq!(
        (stamp.version.as_str(), stamp.anchor.as_str()),
        ("133", "sha256")
    );
    assert_eq!(
        aterm_uds::host::tool("wasm-opt").pinned_in(&m.tooling),
        Some(exe.clone())
    );

    let fetched = host.fetched.borrow().len();
    assert_eq!(provide(&right, &m, &host, &mut |_| {}), State::Present(exe));
    assert_eq!(host.fetched.borrow().len(), fetched);
}

const PWSH_URL: &str = "https://fixture.invalid/powershell-{v}-osx-arm64.tar.gz";

fn pwsh_archive(lib_tail: &[u8], notices: bool) -> Vec<u8> {
    let pwsh = macho(&[LIBSYSTEM], b"TEAM=UBF8T346G9");
    let lib = macho(&[LIBSYSTEM], lib_tail);
    let mut members = vec![
        ("pwsh", Member::File(&pwsh, 0o700)),
        ("LICENSE.txt", Member::File(b"MIT", 0o600)),
        ("Modules/", Member::Dir),
        ("libSystem.Native.dylib", Member::File(&lib, 0o644)),
    ];
    if notices {
        members.push(("ThirdPartyNotices.txt", Member::File(b"notices", 0o644)));
    }
    gzip(&tar(&members))
}

fn pwsh_row(archive: &[u8]) -> Row {
    fixture_row(
        "pwsh",
        "pwsh",
        PWSH_URL,
        &sha256(archive),
        archive.len() as u64,
        |archives| Install::Unpack {
            archives,
            strip: 0,
            anchor: Anchor::DeveloperId {
                team: MICROSOFT_TEAM,
            },
            keep: &["LICENSE.txt", "ThirdPartyNotices.txt"],
            cap: None,
            holds: Holds::Program,
        },
    )
}

#[test]
fn another_teams_mach_o_refuses_pwsh() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let url = PWSH_URL.replace("{v}", "7.6.7");

    let foreign = pwsh_archive(OTHER_TEAM, true);
    let host = Fake::new().serve(&url, foreign.clone());
    match provide(&pwsh_row(&foreign), &m, &host, &mut |_| {}) {
        State::Failed(why) => assert!(
            why.contains("libSystem.Native.dylib is not signed by team UBF8T346G9"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        names(&m.tooling),
        Vec::<String>::new(),
        "nothing left behind"
    );

    let bare = pwsh_archive(b"TEAM=UBF8T346G9", false);
    let host = Fake::new().serve(&url, bare.clone());
    assert!(matches!(
        provide(&pwsh_row(&bare), &m, &host, &mut |_| {}),
        State::Failed(w) if w.contains("ThirdPartyNotices.txt")
    ));
    assert_eq!(names(&m.tooling), Vec::<String>::new());

    let microsoft = pwsh_archive(b"TEAM=UBF8T346G9", true);
    let host = Fake::new().serve(&url, microsoft.clone());
    let dir = m.tooling.join("pwsh-7.6.7");
    assert_eq!(
        provide(&pwsh_row(&microsoft), &m, &host, &mut |_| {}),
        State::Placed(dir.join("pwsh"))
    );
    assert_eq!(host.signed.borrow().len(), 2, "both Mach-Os were checked");
    assert_eq!(mode(&dir.join("pwsh")), 0o755);
    assert_eq!(mode(&dir.join("LICENSE.txt")), 0o644);
    assert!(dir.join("ThirdPartyNotices.txt").is_file());
    assert_eq!(
        place::read_stamp(&dir).unwrap().anchor,
        format!("developer-id {MICROSOFT_TEAM}")
    );
    assert_eq!(names(&m.tooling), ["pwsh-7.6.7"]);
}

#[test]
fn an_unstamped_or_stale_copy_is_replaced() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let url = PWSH_URL.replace("{v}", "7.6.7");
    let archive = pwsh_archive(b"TEAM=UBF8T346G9", true);
    let row = pwsh_row(&archive);
    let want = sha256(&archive);
    let dir = m.tooling.join("pwsh-7.6.7");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pwsh"), "hand-laid").unwrap();
    std::fs::write(dir.join("stray"), "").unwrap();
    assert!(matches!(check(&row, &m, &Fake::new()), State::Missing(_)));
    let host = Fake::new().serve(&url, archive);
    assert_eq!(
        provide(&row, &m, &host, &mut |_| {}),
        State::Placed(dir.join("pwsh"))
    );
    assert!(!dir.join("stray").exists(), "the hand-laid copy went");
    let aside: Vec<String> = names(&m.tooling)
        .into_iter()
        .filter(|n| n != "pwsh-7.6.7")
        .collect();
    assert!(
        matches!(aside.as_slice(), [one] if one.starts_with(".replaced-pwsh-7.6.7-")),
        "the hand-laid copy, which may be running, is kept aside: {aside:?}"
    );
    assert!(m.tooling.join(&aside[0]).join("tree/stray").exists());
    assert!(matches!(check(&row, &m, &Fake::new()), State::Present(_)));

    let mut stale = place::read_stamp(&dir).expect("the run's stamp");
    stale.sha256 = Some("0".repeat(64));
    place::write_stamp(&dir, &stale).unwrap();
    std::fs::write(dir.join("stray"), "").unwrap();
    assert!(matches!(check(&row, &m, &Fake::new()), State::Missing(_)));
    let host = Fake::new().serve(&url, pwsh_archive(b"TEAM=UBF8T346G9", true));
    assert_eq!(
        provide(&row, &m, &host, &mut |_| {}),
        State::Placed(dir.join("pwsh"))
    );
    assert!(!dir.join("stray").exists(), "the stale copy went");
    assert_eq!(place::read_stamp(&dir).and_then(|s| s.sha256), Some(want));
}

const FISH_URL: &str = "https://fixture.invalid/fish-{v}.tar.xz";

fn fish_source() -> Vec<u8> {
    tar(&[
        ("fish-4.9.3/", Member::Dir),
        (
            "fish-4.9.3/Cargo.toml",
            Member::File(b"[package]\nname = \"fish\"\n", 0o644),
        ),
        (
            "fish-4.9.3/Cargo.lock",
            Member::File(b"version = 4\n", 0o644),
        ),
        ("fish-4.9.3/COPYING", Member::File(b"GPLv2", 0o644)),
    ])
}

fn fish_row(source: &[u8]) -> Row {
    let sha = leak(sha256(source));
    Row {
        name: "fish",
        tool: Some("fish"),
        install: Install::Build {
            source: Archive {
                platform: "source",
                url: FISH_URL,
                sha256: sha,
                bytes: source.len() as u64,
            },
            features: &[],
        },
    }
}

#[test]
fn a_fish_build_linking_homebrew_pcre2_is_refused() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let source = fish_source();
    let url = FISH_URL.replace("{v}", "4.9.3");

    let mut host = Fake::new().serve(&url, source.clone());
    host.built_fish = macho(&[LIBSYSTEM, HOMEBREW_PCRE2], b"");
    match provide(&fish_row(&source), &m, &host, &mut |_| {}) {
        State::Failed(why) => assert!(
            why.contains(HOMEBREW_PCRE2) && why.contains("outside /usr/lib and /System"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        names(&m.tooling),
        Vec::<String>::new(),
        "nothing left behind"
    );

    let host = Fake::new().serve(&url, source.clone());
    let dir = m.tooling.join("fish-4.9.3");
    assert_eq!(
        provide(&fish_row(&source), &m, &host, &mut |_| {}),
        State::Placed(dir.join("bin/fish"))
    );
    let stamp = place::read_stamp(&dir).unwrap();
    assert_eq!(stamp.sha256.as_deref(), Some(sha256(&source).as_str()));
    assert_eq!(stamp.reports.as_deref(), Some("fish, version 4.9.3"));
    assert_eq!(stamp.features, Some(Vec::new()));
    assert_eq!(mode(&dir.join("bin/fish")), 0o755);
    assert!(dir.join("COPYING").is_file());
    assert_eq!(names(&m.tooling), ["fish-4.9.3"]);
    assert_eq!(
        aterm_uds::host::tool("fish").pinned_in(&m.tooling),
        Some(dir.join("bin/fish")),
        "where every fish test resolves it"
    );
}

#[test]
fn a_fish_build_answering_another_version_is_refused() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let source = fish_source();
    for answers in [
        "fish, version 4.9.2\n",
        "fish, version 4.9.3-12-gdeadbeef\n",
    ] {
        let mut host = Fake::new().serve(&FISH_URL.replace("{v}", "4.9.3"), source.clone());
        host.fish_answers = answers.into();
        match provide(&fish_row(&source), &m, &host, &mut |_| {}) {
            State::Failed(why) => assert!(why.contains("not version 4.9.3"), "{why}"),
            other => panic!("{answers}: {other:?}"),
        }
        assert_eq!(
            names(&m.tooling),
            Vec::<String>::new(),
            "nothing left behind"
        );
    }

    for (error, panics, answer) in [
        (Some("injected read or wait failure"), false, "unused"),
        (None, false, " \n"),
        (None, true, "unused"),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let m = machine(home.path());
        let mut host = Fake::new().serve(&FISH_URL.replace("{v}", "4.9.3"), source.clone());
        host.fish_error = error;
        host.fish_panics = panics;
        host.fish_answers = answer.into();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            provide(&fish_row(&source), &m, &host, &mut |_| {})
        }));
        if panics {
            assert!(result.is_err(), "the injected probe unwinds");
        } else {
            let Ok(State::Failed(why)) = result else {
                panic!("the unknown probe must refuse placement");
            };
            if let Some(error) = error {
                assert!(
                    why.contains(error),
                    "the original I/O error is retained: {why}"
                );
            }
        }
        let probes = host.spawned("bin/fish");
        assert_eq!(probes.len(), 1);
        let image = &probes[0].program;
        assert_eq!(std::fs::read(image).unwrap(), host.built_fish);
        let scratch = image.parent().unwrap().parent().unwrap().parent().unwrap();
        assert!(scratch.starts_with(&m.tooling));
        assert!(scratch.join("src/Cargo.toml").is_file());
        assert!(scratch.join("target.noindex/release/fish").is_file());
        assert!(!m.tooling.join("fish-4.9.3").exists(), "nothing was placed");
        assert_eq!(names(&m.tooling).len(), 1, "only the owned scratch remains");
    }
}

#[test]
fn the_fish_build_is_the_rows_argv_and_environment() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let source = fish_source();
    let host = Fake::new().serve(&FISH_URL.replace("{v}", "4.9.3"), source.clone());
    assert!(provide(&fish_row(&source), &m, &host, &mut |_| {}).in_place());
    let builds = host.spawned("/stage2/bin/targo");
    assert_eq!(builds.len(), 1, "{builds:#?}");
    let build = &builds[0];
    let manifest = arg_after(build, "--manifest-path");
    let target = arg_after(build, "--target-dir");
    let words: Vec<String> = build
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        words,
        [
            "--unverified",
            "build",
            "--release",
            "--locked",
            "--no-default-features",
            "--manifest-path",
            &manifest.to_string_lossy(),
            "--target-dir",
            &target.to_string_lossy(),
        ]
    );
    assert_eq!(
        build.set,
        [
            ("FISH_BUILD_DOCS".to_string(), OsString::from("0")),
            ("PCRE2_SYS_STATIC".to_string(), OsString::from("1")),
            (
                "CARGO_TARGET_DIR".to_string(),
                target.clone().into_os_string()
            ),
        ]
    );
    assert!(!build.set.iter().any(|(k, _)| k.contains("RUSTFLAGS")));
    for gone in [
        "RUSTFLAGS",
        "RUSTDOCFLAGS",
        "RUSTC",
        "RUSTC_WRAPPER",
        "CC",
        "CFLAGS",
    ] {
        assert!(
            build.remove.iter().any(|k| k == gone),
            "{gone}: {:?}",
            build.remove
        );
    }
    assert!(target.starts_with(&m.tooling) && target.extension().is_some_and(|e| e == "noindex"));
    assert!(manifest.ends_with("src/Cargo.toml"));
    assert!(
        !build.cwd.starts_with(manifest.parent().unwrap()),
        "built from outside the tree"
    );

    let replaced = [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS",
        "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER",
        "CARGO_PROFILE_RELEASE_LTO",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CC",
        "CFLAGS",
        "CFLAGS_aarch64-apple-darwin",
        "TARGET_CC",
        "LDFLAGS",
        "LIBRARY_PATH",
        "MACOSX_DEPLOYMENT_TARGET",
    ];
    let parent: Vec<OsString> = replaced
        .iter()
        .chain(&["CARGO_PKG_NAME", "PATH", "CARGO_TARGET_DIR", "HOME"])
        .map(OsString::from)
        .collect();
    let cmd = build.command(&parent);
    let envs: BTreeMap<String, Option<String>> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    for gone in replaced {
        assert_eq!(envs.get(gone), Some(&None), "{gone} removed: {envs:?}");
    }
    assert_eq!(
        envs.get("CARGO_TARGET_DIR"),
        Some(&Some(target.to_string_lossy().into_owned())),
        "the build's own target dir, set after every CARGO_TARGET_ removal"
    );
    assert_eq!(envs.get("HOME"), None, "inherited");
    assert_eq!(
        envs.get("CARGO_PKG_NAME"),
        Some(&None),
        "cargo run's words removed"
    );
    assert_eq!(envs.get("FISH_BUILD_DOCS"), Some(&Some("0".into())));
    assert_eq!(envs.get("PATH"), None, "the rest is inherited untouched");
}

const BASH_URL: &str = "https://fixture.invalid/bash-5.3.tar.gz";

/// A bash source whose archive lays `configure` before the `aclocal.m4` it is made from,
/// as GNU's bash-5.3.tar.gz does.
fn bash_source() -> Vec<u8> {
    gzip(&tar(&[
        ("bash-5.3/", Member::Dir),
        ("bash-5.3/configure", Member::File(b"#!/bin/sh\n", 0o755)),
        ("bash-5.3/configure.ac", Member::File(b"AC_INIT\n", 0o644)),
        ("bash-5.3/aclocal.m4", Member::File(b"dnl\n", 0o644)),
        ("bash-5.3/y.tab.c", Member::File(b"/* bison */\n", 0o644)),
        ("bash-5.3/parse.y", Member::File(b"%%\n", 0o644)),
        ("bash-5.3/COPYING", Member::File(b"GPLv3", 0o644)),
    ]))
}

fn bash_patch(n: usize) -> (String, Vec<u8>) {
    bash_patch_of(n, &[])
}

/// Patch `n`, which changes `files` in their order.
fn bash_patch_of(n: usize, files: &[&str]) -> (String, Vec<u8>) {
    let changes: String = files.iter().map(|f| format!("+++ {f}\n")).collect();
    (
        format!("https://fixture.invalid/bash53-{n:03}"),
        format!("*** patch {n}\n{changes}").into_bytes(),
    )
}

/// The bash row over `source` and `patches`, each pinned to its own bytes unless
/// `pinned` gives another patch body.
fn bash_row(source: &[u8], patches: &[(String, Vec<u8>)], pinned: Option<&[u8]>) -> Row {
    let patches: Vec<Archive> = patches
        .iter()
        .map(|(url, body)| Archive {
            platform: "source",
            url: leak(url.clone()),
            sha256: leak(sha256(pinned.unwrap_or(body))),
            bytes: body.len() as u64,
        })
        .collect();
    Row {
        name: "bash",
        tool: Some("bash-5.3"),
        install: Install::Configure {
            source: Archive {
                platform: "source",
                url: BASH_URL,
                sha256: leak(sha256(source)),
                bytes: source.len() as u64,
            },
            patches: Box::leak(patches.into_boxed_slice()),
        },
    }
}

fn bash_host(source: &[u8], patches: &[(String, Vec<u8>)]) -> Fake {
    patches.iter().fold(
        Fake::new().serve(BASH_URL, source.to_vec()),
        |host, (url, body)| host.serve(url, body.clone()),
    )
}

#[test]
fn bash_is_built_from_its_source_and_each_patch_in_order_where_the_tests_resolve_it() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let source = bash_source();
    let patches = [bash_patch(1), bash_patch(2)];
    let host = bash_host(&source, &patches);
    let dir = m.tooling.join("bash-5.3.20");
    assert_eq!(
        provide(&bash_row(&source, &patches, None), &m, &host, &mut |_| {}),
        State::Placed(dir.join("bin/bash"))
    );
    assert_eq!(
        aterm_uds::host::tool("bash-5.3").pinned_in(&m.tooling),
        Some(dir.join("bin/bash")),
        "where the bash 5.3 tests resolve it"
    );
    assert_eq!(
        *host.fetched.borrow(),
        [BASH_URL, patches[0].0.as_str(), patches[1].0.as_str()]
    );

    let applied = host.spawned(bash::PATCH);
    assert_eq!(applied.len(), 2, "{applied:#?}");
    let src = &applied[0].cwd;
    assert!(
        src.ends_with("src") && src.starts_with(&m.tooling),
        "{src:?}"
    );
    assert_eq!(
        *host.patched.borrow(),
        [&patches[0].1[..], &patches[1].1[..]].concat(),
        "every patch, in the row's order"
    );
    let configure = host.spawned(bash::SH);
    assert_eq!(configure.len(), 1, "{configure:#?}");
    let mut prefix = OsString::from("--prefix=");
    prefix.push(&dir);
    assert_eq!(
        configure[0].args,
        [
            "./configure".into(),
            prefix,
            "--without-bash-malloc".into(),
            "--disable-nls".into()
        ],
        "no translations: configure would link a libintl it finds on the compiler's own path"
    );
    let make = host.spawned(bash::MAKE);
    assert_eq!(make.len(), 1, "{make:#?}");
    for step in applied.iter().chain(&configure).chain(&make) {
        assert_eq!(&step.cwd, src);
        assert_eq!(
            step.set,
            [("PATH".to_string(), OsString::from(bash::BUILD_PATH))]
        );
        for gone in [
            "CC",
            "CFLAGS",
            "CPPFLAGS",
            "LDFLAGS",
            "CPATH",
            "LIBRARY_PATH",
        ] {
            assert!(step.remove.iter().any(|k| k == gone), "{gone}: {step:?}");
        }
    }

    let stamp = place::read_stamp(&dir).unwrap();
    assert_eq!(stamp.row, "bash");
    assert_eq!(stamp.version, "5.3.20");
    assert_eq!(stamp.sha256.as_deref(), Some(sha256(&source).as_str()));
    assert_eq!(
        stamp.reports.as_deref(),
        Some("GNU bash, version 5.3.20(1)-release (aarch64-apple-darwin27.0.0)")
    );
    assert_eq!(mode(&dir.join("bin/bash")), 0o755);
    assert_eq!(
        std::fs::read(dir.join("bin/bash")).unwrap(),
        host.built_bash
    );
    assert!(dir.join("COPYING").is_file());
    assert_eq!(names(&m.tooling), ["bash-5.3.20"], "no scratch left");
    assert!(matches!(
        check(&bash_row(&source, &patches, None), &m, &Fake::new()),
        State::Present(_)
    ));
}

#[test]
fn a_patch_to_a_generated_files_source_reaches_the_build_and_autoconf_never_runs() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let source = bash_source();
    // bash52-018 changed `parse.y` alone; bash53-016 changes `configure.ac`, then `configure`.
    let patches = [
        bash_patch_of(1, &["parse.y"]),
        bash_patch_of(2, &["configure.ac", "configure"]),
    ];
    let host = bash_host(&source, &patches);
    let state = provide(&bash_row(&source, &patches, None), &m, &host, &mut |_| {});
    assert!(matches!(state, State::Placed(_)), "{state:?}");
    assert_eq!(
        String::from_utf8_lossy(&host.parser.borrow()),
        String::from_utf8_lossy(&patches[0].1),
        "make ran the patched parse.y through bison"
    );
}

#[test]
fn a_bash_patch_off_its_pin_or_that_does_not_apply_refuses_the_row() {
    let source = bash_source();
    let patches = [bash_patch(1), bash_patch(2)];
    let rejected = [bash_patch(1), (bash_patch(2).0, b"*** REJECT\n".to_vec())];
    for (row, served, why) in [
        (
            bash_row(&source, &patches, Some(&b"another patch"[..])),
            &patches,
            "the archive changed under its version",
        ),
        (
            bash_row(&source, &rejected, None),
            &rejected,
            "bash53-002 did not apply",
        ),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let m = machine(home.path());
        let host = bash_host(&source, served);
        match provide(&row, &m, &host, &mut |_| {}) {
            State::Failed(said) => assert!(said.contains(why), "{said}"),
            other => panic!("{why}: {other:?}"),
        }
        assert!(host.spawned(bash::SH).is_empty(), "nothing was configured");
        assert_eq!(
            names(&m.tooling),
            Vec::<String>::new(),
            "nothing left behind"
        );
    }
}

#[test]
fn a_bash_build_that_is_not_the_pin_is_refused() {
    let source = bash_source();
    let patches = [bash_patch(1)];
    for (answers, built, why) in [
        (
            "GNU bash, version 5.3.19(1)-release (aarch64-apple-darwin27.0.0)\n",
            None,
            "not version 5.3.20",
        ),
        (
            "GNU bash, version 5.3.200(1)-release (aarch64-apple-darwin27.0.0)\n",
            None,
            "not version 5.3.20",
        ),
        (
            "GNU bash, version 5.3.20(1)-release (aarch64-apple-darwin27.0.0)\n",
            Some(macho(
                &[LIBSYSTEM, "/opt/homebrew/lib/libintl.8.dylib"],
                b"",
            )),
            "/opt/homebrew/lib/libintl.8.dylib from outside /usr/lib and /System",
        ),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let m = machine(home.path());
        let mut host = bash_host(&source, &patches);
        host.bash_answers = answers.into();
        if let Some(built) = built {
            host.built_bash = built;
        }
        match provide(&bash_row(&source, &patches, None), &m, &host, &mut |_| {}) {
            State::Failed(said) => assert!(said.contains(why), "{said}"),
            other => panic!("{answers}: {other:?}"),
        }
        assert!(
            !m.tooling.join("bash-5.3.20").exists(),
            "nothing was placed"
        );
    }
}

fn all_in_place(m: &Machine) {
    for row in select(&Options::default()) {
        let Some(t) = target(row, m) else {
            continue;
        };
        let exe = t.dir.join(&t.exe);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "#!/bin/false\n").unwrap();
        place::write_stamp(&t.dir, &t.stamp(row.name, None, "sha256", None)).unwrap();
    }
}

#[test]
fn record_alone_fetches_wasm_bindgen() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    all_in_place(&m);
    let host = Fake::new();
    let mut said = Vec::new();
    let code = run(&Options::default(), &m, &host, &mut |l| {
        said.push(l.to_string())
    });
    assert_eq!(code, crate::exit::PASS, "{said:#?}");
    assert!(host.fetched.borrow().is_empty() && host.spawns.borrow().is_empty());
    assert!(
        !said.iter().any(|l| l.contains("wasm-bindgen")),
        "{said:#?}"
    );
    let picked = |opts: &Options| -> Vec<&str> { select(opts).iter().map(|r| r.name).collect() };
    let linux = Options {
        linux: true,
        ..Options::default()
    };
    assert!(!picked(&Options::default()).contains(&"wasm-bindgen"));
    assert!(!picked(&linux).contains(&"wasm-bindgen") && picked(&linux).contains(&"lima"));

    let record = Options {
        record: true,
        ..Options::default()
    };
    assert_eq!(run(&record, &m, &host, &mut |_| {}), crate::exit::PASS);
    assert!(host.fetched.borrow().is_empty(), "the rest stayed in place");
    let installs = host.spawned("/stage2/bin/targo");
    assert_eq!(installs.len(), 1, "{installs:#?}");
    let words: Vec<String> = installs[0]
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let pin = crate::wasm_glue::BINDGEN;
    assert_eq!(
        words[..7],
        [
            "--unverified",
            "install",
            "wasm-bindgen-cli",
            "--version",
            pin,
            "--locked",
            "--force"
        ]
    );
    let dir = m.tooling.join(format!("wasm-bindgen-{pin}"));
    assert!(dir.join("bin/wasm-bindgen").is_file());
    let stamp = place::read_stamp(&dir).unwrap();
    assert!(!stamp.adopted && stamp.reports == Some(format!("wasm-bindgen {pin}")));
    assert_eq!(
        names(&m.tooling)
            .iter()
            .filter(|n| n.starts_with('.'))
            .count(),
        0
    );
}

#[test]
fn a_bindgen_found_in_place_is_rebuilt_and_never_run() {
    let pin = crate::wasm_glue::BINDGEN;
    let row = super::row("wasm-bindgen").unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let dir = m.tooling.join(format!("wasm-bindgen-{pin}"));
    let found = dir.join("bin/wasm-bindgen");
    std::fs::create_dir_all(found.parent().unwrap()).unwrap();
    std::fs::write(&found, "#!/bin/false\n").unwrap();
    std::fs::write(
        dir.join(".crates2.json"),
        format!(
            "{{\"installs\":{{\"wasm-bindgen-cli {pin} (registry+https://github.com/rust-lang/crates.io-index)\":{{}}}}}}"
        ),
    )
    .unwrap();
    assert!(matches!(check(row, &m, &Fake::new()), State::Missing(_)));
    let host = Fake::new();
    assert_eq!(
        provide(row, &m, &host, &mut |_| {}),
        State::Placed(found.clone())
    );
    assert_eq!(
        host.spawned("/stage2/bin/targo").len(),
        1,
        "built at the pin"
    );
    let ran: Vec<PathBuf> = host
        .spawns
        .borrow()
        .iter()
        .map(|s| s.program.clone())
        .collect();
    assert!(
        !ran.contains(&found),
        "the copy found in place never ran: {ran:#?}"
    );
    assert!(
        ran.iter().any(|p| p.ends_with("bin/wasm-bindgen")
            && p.to_string_lossy().contains(place::SCRATCH_PREFIX)),
        "the build was asked its version in its scratch: {ran:#?}"
    );
    let stamp = place::read_stamp(&dir).unwrap();
    assert!(!stamp.adopted);
    assert_eq!(stamp.anchor, "crates.io");
    assert!(!dir.join(".crates2.json").exists(), "the found copy went");
    assert!(matches!(check(row, &m, &Fake::new()), State::Present(_)));
}

#[test]
fn check_changes_nothing() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let host = Fake::new();
    let opts = Options {
        check: true,
        ..Options::default()
    };
    let mut said = Vec::new();
    assert_eq!(
        run(&opts, &m, &host, &mut |l| said.push(l.to_string())),
        crate::exit::FAILED
    );
    assert!(
        said.iter().any(|l| l.starts_with("MISSING — fish 4.9.3")),
        "{said:#?}"
    );
    assert!(host.fetched.borrow().is_empty() && host.spawns.borrow().is_empty());
    assert!(!m.tooling.exists() && !m.browser.exists());
}

fn browser_fixture(m: &Machine, zip: &[u8]) -> (crate::browser::Pin, Target) {
    let pin = crate::browser::Pin {
        what: "chrome-headless-shell 0.0.1".into(),
        version: "0.0.1".into(),
        url: "https://fixture.invalid/chrome-headless-shell-mac-arm64.zip".into(),
        sha256: leak(sha256(zip)),
        bytes: zip.len() as u64,
        dir: m.browser.join("chrome-headless-shell-0.0.1-mac-arm64"),
        binary: PathBuf::from("chrome-headless-shell-mac-arm64/chrome-headless-shell"),
    };
    let t = Target {
        dir: pin.dir.clone(),
        exe: pin.binary.to_string_lossy().into_owned(),
        version: pin.version.clone(),
        sha256: Some(pin.sha256.to_string()),
        platform: m.platform.clone(),
        features: None,
        sizes: Vec::new(),
    };
    (pin, t)
}

#[test]
fn a_browser_is_placed_where_the_lane_looks() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let row = super::row("chrome-headless-shell").unwrap();
    let real = crate::browser::pin_for(Engine::Chromium, "macos", "aarch64", &m.browser).unwrap();
    let host = Fake::new().serve(&real.url, b"not chrome".to_vec());
    assert!(
        matches!(provide(row, &m, &host, &mut |_| {}), State::Failed(w) if w.contains("bytes"))
    );
    assert!(host.spawns.borrow().is_empty(), "nothing unzipped");
    assert_eq!(names(&m.browser), Vec::<String>::new());

    let zip = b"a fixture zip".to_vec();
    let (pin, t) = browser_fixture(&m, &zip);
    let exe = t.dir.join(&t.exe);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "hand-laid").unwrap();
    std::fs::write(t.dir.join("stray"), "").unwrap();
    assert!(!complete(&t));
    let host = Fake::new().serve(&pin.url, zip.clone());
    place_browser("chrome-headless-shell", &pin, &t, &m, &host, &mut |_| {}).unwrap();
    assert!(complete(&t));
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        b"#!/bin/false\n",
        "the pin's binary"
    );
    assert!(
        !t.dir.join("stray").exists(),
        "the copy found in place went"
    );
    let stamp = place::read_stamp(&t.dir).unwrap();
    assert_eq!(stamp.platform.as_deref(), Some("macos-aarch64"));
    assert_eq!(stamp.sha256.as_deref(), Some(pin.sha256));
    let (into, cap) = host.fetched_into.borrow()[0].clone();
    assert!(
        into.starts_with(&m.tooling)
            && into
                .to_string_lossy()
                .contains(&format!("{}chrome-headless-shell-", place::SCRATCH_PREFIX)),
        "the download's scratch is the tooling cache's: {}",
        into.display()
    );
    assert_eq!(
        cap,
        zip.len() as u64,
        "the fetch is capped at the pin's size"
    );
    let kept = names(&m.tooling);
    assert!(
        matches!(kept.as_slice(), [one] if one.starts_with(".replaced-chrome-headless-shell-")),
        "the scratch went; the hand-laid copy, which may be running, is kept aside: {kept:?}"
    );
    assert_eq!(names(&m.browser), ["chrome-headless-shell-0.0.1-mac-arm64"]);
}

#[test]
fn the_drivers_browser_is_only_the_pin_provision_placed() {
    use crate::browser::chromium::pinned_from;
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let t = target(row("chrome-headless-shell").unwrap(), &m).expect("pinned for macos-aarch64");
    let exe = t.dir.join(&t.exe);
    let none = |why: &str| {
        let refused = pinned_from(None, Some(&m)).expect_err(why);
        assert!(
            refused.contains("no pinned chrome-headless-shell")
                && refused.contains("an installed Chrome is not the pin"),
            "{why}: {refused}"
        );
    };
    none("nothing placed");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "hand-laid").unwrap();
    none("a copy laid by hand");
    let mut other = t.stamp("chrome-headless-shell", None, "sha256", None);
    other.version = "0.0.1".into();
    place::write_stamp(&t.dir, &other).unwrap();
    none("another pin's stamp");
    let mut adopted = t.stamp("chrome-headless-shell", None, "sha256", None);
    adopted.adopted = true;
    place::write_stamp(&t.dir, &adopted).unwrap();
    none("an adopted copy");
    place::write_stamp(
        &t.dir,
        &t.stamp("chrome-headless-shell", None, "sha256", None),
    )
    .unwrap();
    let found = pinned_from(None, Some(&m)).expect("the placed pin");
    assert_eq!((found.path.as_path(), found.shell), (exe.as_path(), true));
    assert!(found.source.starts_with("pinned chrome-headless-shell"));

    let named = pinned_from(Some(home.path().join("rc").into()), None).expect_err("no file");
    assert!(
        named.contains("ATERM_BROWSER names") && named.contains("not a file"),
        "{named}"
    );
    let chrome = home.path().join("Chromium");
    std::fs::write(&chrome, "").unwrap();
    let named = pinned_from(Some(chrome.clone().into()), Some(&m)).expect("the override");
    assert_eq!(
        (named.path, named.source.as_str()),
        (chrome, "ATERM_BROWSER")
    );
    assert!(pinned_from(None, None).unwrap_err().contains("no $HOME"));
}

#[test]
fn a_browser_zip_with_a_link_out_of_its_tree_is_refused() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let zip = b"a fixture zip".to_vec();
    let (pin, t) = browser_fixture(&m, &zip);
    let victim = home.path().join("owner-file.txt");
    std::fs::write(&victim, "the owner's").unwrap();
    for escape in [victim.clone(), PathBuf::from("../../download")] {
        let mut host = Fake::new().serve(&pin.url, zip.clone());
        host.zip_links = vec![("chrome-headless-shell-mac-arm64/escape", escape.clone())];
        let why =
            place_browser("chrome-headless-shell", &pin, &t, &m, &host, &mut |_| {}).unwrap_err();
        assert!(
            why.contains("chrome-headless-shell-mac-arm64/escape leads outside its tree"),
            "{}: {why}",
            escape.display()
        );
        assert_eq!(names(&m.browser), Vec::<String>::new(), "nothing placed");
        assert_eq!(names(&m.tooling), Vec::<String>::new(), "no scratch left");
    }
    let mut host = Fake::new().serve(&pin.url, zip.clone());
    host.zip_links = vec![(
        "chrome-headless-shell-mac-arm64/loop",
        PathBuf::from("loop"),
    )];
    let why = place_browser("chrome-headless-shell", &pin, &t, &m, &host, &mut |_| {}).unwrap_err();
    assert!(why.contains("leads to nothing it holds"), "{why}");

    let mut host = Fake::new().serve(&pin.url, zip);
    host.zip_links = vec![(
        "stamp.json",
        PathBuf::from("chrome-headless-shell-mac-arm64/chrome-headless-shell"),
    )];
    place_browser("chrome-headless-shell", &pin, &t, &m, &host, &mut |_| {}).unwrap();
    let stamp = t.dir.join(STAMP);
    assert!(std::fs::symlink_metadata(&stamp).unwrap().is_file());
    assert_eq!(
        std::fs::read(t.dir.join(&t.exe)).unwrap(),
        b"#!/bin/false\n",
        "the stamp went to its own file, not through the link"
    );
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "the owner's");
    assert!(complete(&t));
}

#[test]
fn a_host_nothing_is_pinned_for_could_not_run() {
    let home = aterm_tempfile::tempdir().unwrap();
    let mut m = machine(home.path());
    m.platform = "linux-riscv64".into();
    let opts = Options {
        rows: vec!["fish".into(), "pwsh".into(), "webkit".into()],
        ..Options::default()
    };
    assert_eq!(
        run(&opts, &m, &Fake::new(), &mut |_| {}),
        crate::exit::COULD_NOT_RUN
    );
}

#[test]
fn the_command_line_names_rows_and_flags_only() {
    let o = parse(&["--linux".into(), "fish".into(), "fish".into()]).unwrap();
    assert!(o.linux && o.rows == ["fish"]);
    assert!(parse(&["--fetch".into()]).is_err());
    assert!(
        parse(&["fish-shell".into()])
            .unwrap_err()
            .contains("the rows: rustup, fish")
    );
    let names: Vec<&str> = select(&Options::default()).iter().map(|r| r.name).collect();
    assert_eq!(
        names,
        [
            "rustup",
            "fish",
            "bash",
            "pwsh",
            "binaryen",
            "chrome-headless-shell",
            "firefox",
            "webkit"
        ]
    );
}

#[test]
fn each_row_places_what_its_tool_row_resolves() {
    for row in ROWS {
        let Some(tool) = row.tool_row() else {
            continue;
        };
        if let Some(pin) = tool.pin {
            for platform in ["macos-aarch64", "macos-x86_64"] {
                if let Some(a) = row.archive_for(platform) {
                    let named = match row.install {
                        Install::Configure { .. } => pin.version.rsplit_once('.').unwrap().0,
                        _ => pin.version,
                    };
                    assert!(a.url_at(pin.version).contains(named), "{}", a.url);
                    assert_eq!(a.sha256.len(), 64);
                }
            }
            match row.install {
                Install::Configure { source, patches } => {
                    let (release, level) = pin.version.rsplit_once('.').unwrap();
                    assert_eq!(
                        patches.len().to_string(),
                        level,
                        "{}: the version's last part counts the patches",
                        row.name
                    );
                    let flat: String = release.chars().filter(|c| *c != '.').collect();
                    for (n, patch) in patches.iter().enumerate() {
                        assert!(
                            patch.url.ends_with(&format!(
                                "/bash-{release}-patches/bash{flat}-{:03}",
                                n + 1
                            )),
                            "{}: patch {} is {}",
                            row.name,
                            n + 1,
                            patch.url
                        );
                        assert_eq!(patch.sha256.len(), 64);
                        assert_eq!(patch.platform, "source");
                    }
                    assert!(source.url.ends_with(&format!("/bash-{release}.tar.gz")));
                    for host in pin.hosts {
                        assert!(row.archive_for(host).is_some(), "{} on {host}", row.name);
                    }
                    assert!(row.archive_for("linux-riscv64").is_none());
                }
                Install::Unpack { archives, .. } => {
                    let mut hosts: Vec<&str> = archives.iter().map(|a| a.platform).collect();
                    hosts.sort_unstable();
                    let mut pinned = pin.hosts.to_vec();
                    pinned.sort_unstable();
                    assert_eq!(hosts, pinned, "{}", row.name);
                }
                Install::Build { .. } => {
                    for host in pin.hosts {
                        assert!(row.archive_for(host).is_some(), "{} on {host}", row.name);
                    }
                    assert!(row.archive_for("linux-riscv64").is_none());
                }
                _ => {}
            }
        }
    }
}

#[test]
fn a_rustup_init_off_its_pin_never_runs() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    std::fs::remove_file(home.path().join(".cargo/bin/rustup")).unwrap();
    let init = b"#!/bin/false\nrustup-init\n".to_vec();
    let url = "https://fixture.invalid/rustup/archive/{v}/aarch64-apple-darwin/rustup-init";
    let pinned = |sha256: &str| Row {
        name: "rustup",
        tool: Some("rustup"),
        install: Install::Rustup {
            version: "1.29.2",
            init: Box::leak(Box::new([Archive {
                platform: "macos-aarch64",
                url,
                sha256: leak(sha256.to_string()),
                bytes: init.len() as u64,
            }])),
        },
    };
    let served = url.replace("{v}", "1.29.2");

    let mut host = Fake::new().serve(&served, init.clone());
    host.bare = Some(Bare::default());
    match provide(
        &pinned(&sha256(b"another rustup-init")),
        &m,
        &host,
        &mut |_| {},
    ) {
        State::Failed(why) => assert!(
            why.contains("rustup was not installed") && why.contains("hashes to"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(
        host.bare.as_ref().unwrap().calls.borrow().is_empty(),
        "nothing ran"
    );
    assert_eq!(
        names(&m.tooling),
        Vec::<String>::new(),
        "nothing left behind"
    );

    let mut host = Fake::new().serve(&served, init.clone());
    host.bare = Some(Bare::default());
    assert_eq!(
        provide(&pinned(&sha256(&init)), &m, &host, &mut |_| {}),
        State::Placed(m.homes.rustup_home.clone())
    );
    let calls = host.bare.as_ref().unwrap().calls.borrow().clone();
    let (program, executable, args) = &calls[0];
    assert!(
        program.starts_with(&m.tooling)
            && program
                .to_string_lossy()
                .contains(&format!("{}rustup-", place::SCRATCH_PREFIX))
            && program.ends_with("rustup-init"),
        "{}",
        program.display()
    );
    assert!(*executable, "rustup-init ran executable");
    assert_eq!(
        args,
        "-y --no-modify-path --profile minimal --default-toolchain none"
    );
    assert_eq!(host.fetched.borrow().as_slice(), [served]);
    assert_eq!(names(&m.tooling), Vec::<String>::new(), "the scratch went");
}

#[test]
fn rustup_init_is_pinned_for_every_host_rustup_serves() {
    let row = super::row("rustup").unwrap();
    let Install::Rustup { version, init } = row.install else {
        panic!("{row:?}");
    };
    for platform in [
        "macos-aarch64",
        "macos-x86_64",
        "linux-x86_64",
        "linux-aarch64",
    ] {
        let a = row.archive_for(platform).expect(platform);
        assert!(
            a.url_at(version).contains(&format!("/archive/{version}/")),
            "{}",
            a.url
        );
        assert_eq!(a.sha256.len(), 64);
    }
    assert_eq!(init.len(), 4);
}

#[test]
fn a_tree_without_publish_wants_no_wasm32_pin() {
    let home = aterm_tempfile::tempdir().unwrap();
    let mut m = machine(home.path());
    let row = super::row("rustup").unwrap();
    let public = home.path().join("public-clone");
    std::fs::create_dir_all(&public).unwrap();
    m.repo = public.clone();
    let host = Fake::new();
    assert!(
        matches!(check(row, &m, &host), State::Present(_)),
        "{:?}",
        check(row, &m, &host)
    );
    assert_eq!(
        provide(row, &m, &host, &mut |_| {}),
        State::Present(m.homes.rustup_home.clone())
    );

    std::fs::create_dir_all(public.join("publish")).unwrap();
    std::fs::write(
        public.join("publish/config.sh"),
        "CHECK_CMD_DEFAULT=\"true\"\n",
    )
    .unwrap();
    match check(row, &m, &host) {
        State::Missing(why) => assert!(why.contains("no wasm32 clause"), "{why}"),
        other => panic!("{other:?}"),
    }
    match provide(row, &m, &host, &mut |_| {}) {
        State::Failed(why) => assert!(why.contains("the wasm32 pin is not acquired"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn only_linux_holds_the_rustup_row_to_the_guest_agents_musl_std() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let row = super::row("rustup").unwrap();
    let host = Fake::new();
    assert!(
        matches!(check(row, &m, &host), State::Present(_)),
        "{:?}",
        check(row, &m, &host)
    );
    let linux = parse(&["--linux".to_string()]).unwrap();
    match check_for(row, &linux, &m, &host) {
        State::Missing(why) => assert!(
            why.contains("rustup target add --toolchain stable aarch64-unknown-linux-musl"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
}

/// A run that provides (no `--check`) carries `--linux` down to stable: on a machine
/// whose stable has every target but the guest agent's musl std, a plain run finds the
/// rustup row in place and a `--linux` run asks stable for that std. This rustup refuses
/// every change, so the add it was asked for is named in the run's lines.
#[test]
fn a_linux_run_asks_stable_for_the_guest_agents_musl_std_and_a_plain_run_does_not() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let host = Fake::new();
    let add = "rustup target add --toolchain stable aarch64-unknown-linux-musl";
    let asked = |said: &[String]| {
        said.iter()
            .any(|l| l.contains("aarch64-unknown-linux-musl"))
    };

    let mut said = Vec::new();
    let plain = parse(&["rustup".to_string()]).unwrap();
    assert_eq!(
        run(&plain, &m, &host, &mut |l| said.push(l.to_string())),
        crate::exit::PASS,
        "{said:#?}"
    );
    assert!(!asked(&said), "{said:#?}");

    let mut said = Vec::new();
    let linux = parse(&["--linux".to_string(), "rustup".to_string()]).unwrap();
    assert!(!linux.check);
    assert_eq!(
        run(&linux, &m, &host, &mut |l| said.push(l.to_string())),
        crate::exit::FAILED,
        "{said:#?}"
    );
    assert!(
        said.iter().any(|l| l.starts_with("FAIL — rustup: `")
            && l.contains(&format!("/rustup {}` failed", &add["rustup ".len()..]))),
        "`{add}` was asked for: {said:#?}"
    );
}

#[test]
fn a_stamp_for_another_host_or_build_is_not_the_pin() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    all_in_place(&m);
    let fish = super::row("fish").unwrap();
    let t = target(fish, &m).unwrap();
    assert!(matches!(check(fish, &m, &Fake::new()), State::Present(_)));
    let good = place::read_stamp(&t.dir).unwrap();
    assert_eq!(good.features, Some(Vec::new()));
    type Edit = fn(&mut Stamp);
    let edits: [(&str, Edit); 5] = [
        ("another host", |s| s.platform = Some("macos-x86_64".into())),
        ("no host", |s| s.platform = None),
        ("other features", |s| {
            s.features = Some(vec!["embed-manpages".into()])
        }),
        ("adopted", |s| s.adopted = true),
        ("another version", |s| s.version = "4.9.2".into()),
    ];
    for (what, edit) in edits {
        let mut stamp = good.clone();
        edit(&mut stamp);
        place::write_stamp(&t.dir, &stamp).unwrap();
        assert!(
            matches!(check(fish, &m, &Fake::new()), State::Missing(_)),
            "{what} is not the pin"
        );
    }
    place::write_stamp(&t.dir, &good).unwrap();
    assert!(complete(&t));
}

#[test]
fn a_row_found_in_place_is_marked_used() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    all_in_place(&m);
    let row = super::row("pwsh").unwrap();
    let stamp = target(row, &m).unwrap().dir.join(STAMP);
    let month = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 24 * 3600);
    let age = |at| {
        std::fs::File::options()
            .append(true)
            .open(&stamp)
            .unwrap()
            .set_modified(at)
            .unwrap();
    };
    let modified = || std::fs::metadata(&stamp).unwrap().modified().unwrap();
    age(month);
    assert!(matches!(check(row, &m, &Fake::new()), State::Present(_)));
    assert_eq!(modified(), month, "--check changes nothing");
    assert!(matches!(
        provide(row, &m, &Fake::new(), &mut |_| {}),
        State::Present(_)
    ));
    assert!(
        modified() > month + std::time::Duration::from_secs(30 * 24 * 3600),
        "the stamp was marked used"
    );
}

#[test]
fn the_download_is_capped_at_the_pins_size_and_reads_no_curlrc() {
    let dest = Path::new("/scratch/.provision-x/download/a.tar.gz");
    let spawn = fetch_spawn("https://example.invalid/a.tar.gz", dest, 2_628_460);
    let words: Vec<String> = spawn
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(spawn.program, Path::new(CURL));
    assert_eq!(words[0], "-q", "{words:?}");
    let cap = words
        .iter()
        .position(|w| w == "--max-filesize")
        .expect("a cap");
    assert_eq!(words[cap + 1], "2628460");
    for (flag, value) in [
        ("--proto", "=https"),
        ("--proto-redir", "=https"),
        ("-o", "/scratch/.provision-x/download/a.tar.gz"),
    ] {
        let i = words.iter().position(|w| w == flag).expect(flag);
        assert_eq!(words[i + 1], value, "{flag}");
    }
    assert_eq!(
        words.last().map(String::as_str),
        Some("https://example.invalid/a.tar.gz")
    );
}

#[test]
fn a_translated_run_provisions_the_macs_own_rows() {
    assert_eq!(platform_of("macos", "x86_64", true), "macos-aarch64");
    assert_eq!(platform_of("macos", "x86_64", false), "macos-x86_64");
    assert_eq!(platform_of("macos", "aarch64", false), "macos-aarch64");
    assert_eq!(platform_of("linux", "x86_64", true), "linux-x86_64");
}

const ROOT_URL: &str = "https://fixture.invalid/release-{v}/root.tar.gz";

/// A fixture of the root row's shape: one plain member, an image of 3000 bytes, under `cap`.
fn root_row(archive: &[u8], cap: u64) -> Row {
    fixture_row(
        "ubuntu-root",
        "ubuntu-root",
        ROOT_URL,
        &sha256(archive),
        archive.len() as u64,
        |archives| Install::Unpack {
            archives,
            strip: 0,
            anchor: Anchor::Sha256,
            keep: &[aterm_vz::pins::ROOT_MEMBER],
            cap: Some(cap),
            holds: Holds::Image { bytes: 3000 },
        },
    )
}

#[test]
fn the_root_image_is_unpacked_read_only_under_its_rows_own_cap() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let ext4 = vec![0x53u8; 3000];
    let archive = gzip(&tar(&[(
        aterm_vz::pins::ROOT_MEMBER,
        Member::File(&ext4, 0o644),
    )]));
    let url = ROOT_URL.replace("{v}", aterm_vz::pins::RELEASE);
    let host = Fake::new().serve(&url, archive.clone());

    match provide(&root_row(&archive, 2999), &m, &host, &mut |_| {}) {
        State::Failed(why) => assert!(why.contains("did not unpack"), "{why}"),
        other => panic!("a member past its row's cap: {other:?}"),
    }
    assert_eq!(
        names(&m.tooling),
        Vec::<String>::new(),
        "nothing left behind"
    );

    let image = m
        .tooling
        .join(format!("ubuntu-noble-root-{}", aterm_vz::pins::RELEASE))
        .join(aterm_vz::pins::ROOT_MEMBER);
    assert_eq!(
        provide(&root_row(&archive, 6000), &m, &host, &mut |_| {}),
        State::Placed(image.clone())
    );
    assert_eq!(std::fs::read(&image).unwrap(), ext4);
    assert_eq!(mode(&image), 0o444, "base is never written");
    assert_eq!(
        aterm_vz::pins::root_tool().placed_in(&m.tooling),
        Some(image.clone()),
        "where the lab finds it"
    );
    let stamp = place::read_stamp(image.parent().unwrap()).unwrap();
    assert_eq!(stamp.sha256.as_deref(), Some(sha256(&archive).as_str()));
    let fetched = host.fetched.borrow().len();
    assert_eq!(
        provide(&root_row(&archive, 6000), &m, &host, &mut |_| {}),
        State::Present(image)
    );
    assert_eq!(
        host.fetched.borrow().len(),
        fetched,
        "a placed base is kept"
    );

    let two = gzip(&tar(&[(
        aterm_vz::pins::ROOT_MEMBER,
        Member::File(&ext4, 0o755),
    )]));
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let host = Fake::new().serve(&url, two.clone());
    let placed = provide(&root_row(&two, 6000), &m, &host, &mut |_| {});
    let State::Placed(image) = placed else {
        panic!("{placed:?}")
    };
    assert_eq!(
        mode(&image),
        0o444,
        "an executable member is made read-only too"
    );
}

/// A fixture URL ending in `file`'s own name, as the real row's do.
fn fixture_url(file: aterm_vz::pins::File) -> &'static str {
    leak(format!(
        "https://fixture.invalid/release-{{v}}/unpacked/{}",
        file.name()
    ))
}

fn kernel_row(files: [(&[u8], String); 2]) -> Row {
    let urls = [
        fixture_url(aterm_vz::pins::VMLINUZ),
        fixture_url(aterm_vz::pins::INITRD),
    ];
    let files: Vec<Archive> = urls
        .into_iter()
        .zip(files)
        .map(|(url, (bytes, sha256))| Archive {
            platform: "macos-aarch64",
            url,
            sha256: leak(sha256),
            bytes: bytes.len() as u64,
        })
        .collect();
    Row {
        name: "ubuntu-kernel",
        tool: Some("ubuntu-kernel"),
        install: Install::Files {
            files: Box::leak(files.into_boxed_slice()),
        },
    }
}

#[test]
fn a_files_row_holds_each_file_to_its_pin_and_keeps_them_together() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let at = |f| fixture_url(f).replace("{v}", aterm_vz::pins::RELEASE);
    let (vmlinuz, initrd) = (b"gzip kernel".to_vec(), b"an initrd".to_vec());
    let host = Fake::new()
        .serve(&at(aterm_vz::pins::VMLINUZ), vmlinuz.clone())
        .serve(&at(aterm_vz::pins::INITRD), initrd.clone());

    let off = kernel_row([
        (&vmlinuz, sha256(&vmlinuz)),
        (&initrd, sha256(b"another initrd")),
    ]);
    match provide(&off, &m, &host, &mut |_| {}) {
        State::Failed(why) => assert!(why.contains("hashes to"), "{why}"),
        other => panic!("the second file off its pin: {other:?}"),
    }
    assert_eq!(
        names(&m.tooling),
        Vec::<String>::new(),
        "nothing left behind"
    );

    let row = kernel_row([(&vmlinuz, sha256(&vmlinuz)), (&initrd, sha256(&initrd))]);
    let dir = aterm_vz::pins::kernel_tool()
        .pinned_root(&m.tooling)
        .unwrap();
    let exe = dir.join(aterm_vz::pins::VMLINUZ.name());
    assert_eq!(
        provide(&row, &m, &host, &mut |_| {}),
        State::Placed(exe.clone())
    );
    assert_eq!(
        names(&dir),
        [
            "stamp.json",
            aterm_vz::pins::INITRD.name(),
            aterm_vz::pins::VMLINUZ.name(),
        ]
    );
    for (name, bytes) in [
        (aterm_vz::pins::VMLINUZ.name(), &vmlinuz),
        (aterm_vz::pins::INITRD.name(), &initrd),
    ] {
        assert_eq!(std::fs::read(dir.join(name)).unwrap(), *bytes);
        assert_eq!(mode(&dir.join(name)), 0o444, "{name}");
    }
    let lookup = aterm_uds::host::Lookup {
        named: None,
        tooling: Some(m.tooling.clone()),
        path: None,
    };
    assert_eq!(
        aterm_vz::pins::kernel_tool().find_in(&lookup),
        Some(exe.clone()),
        "where the lab finds the kernel, the initrd beside it"
    );
    let parents: Vec<PathBuf> = host
        .fetched_into
        .borrow()
        .iter()
        .map(|(p, _)| p.parent().unwrap().to_path_buf())
        .collect();
    assert!(
        parents.windows(2).all(|w| w[0] != w[1]),
        "each file lands in a download directory of its own: {parents:?}"
    );
    let fetched = host.fetched.borrow().len();
    assert_eq!(provide(&row, &m, &host, &mut |_| {}), State::Present(exe));
    assert_eq!(host.fetched.borrow().len(), fetched);

    let bumped = kernel_row([
        (&vmlinuz, sha256(&vmlinuz)),
        (b"an initrz", sha256(b"an initrz")),
    ]);
    assert!(
        matches!(check(&bumped, &m, &host), State::Missing(_)),
        "a changed pin of either file is another row's stamp"
    );
}

#[test]
fn the_linux_lanes_image_rows_are_ubuntus_pinned_files() {
    let root = row("ubuntu-root").unwrap();
    let Install::Unpack {
        archives,
        strip,
        anchor,
        keep,
        cap,
        holds,
    } = root.install
    else {
        panic!("{root:?}")
    };
    assert_eq!(
        archives,
        [Archive {
            platform: "macos-aarch64",
            url: aterm_vz::pins::ROOT_ARCHIVE.url,
            sha256: aterm_vz::pins::ROOT_ARCHIVE.sha256,
            bytes: aterm_vz::pins::ROOT_ARCHIVE.bytes,
        }]
    );
    assert_eq!(
        (strip, anchor, holds),
        (
            0,
            Anchor::Sha256,
            Holds::Image {
                bytes: aterm_vz::pins::ROOT_MEMBER_BYTES
            }
        )
    );
    assert_eq!(keep, [aterm_vz::pins::ROOT_MEMBER]);
    assert!(
        cap.is_some_and(|c| c >= 2 * aterm_vz::pins::ROOT_MEMBER_BYTES),
        "the root row's cap is at least twice its member: {cap:?}"
    );
    const {
        assert!(
            aterm_vz::pins::ROOT_MEMBER_BYTES > place::UNPACKED_CAP / 2,
            "why it names its own: the member is past half the shared cap"
        );
    }
    let kernel = row("ubuntu-kernel").unwrap();
    let names: Vec<(&str, &str)> = kernel
        .files_for("macos-aarch64")
        .iter()
        .map(|f| (f.url, f.sha256))
        .collect();
    assert_eq!(
        names,
        [
            (aterm_vz::pins::VMLINUZ.url, aterm_vz::pins::VMLINUZ.sha256),
            (aterm_vz::pins::INITRD.url, aterm_vz::pins::INITRD.sha256),
        ]
    );
    assert!(kernel.files_for("macos-x86_64").is_empty());
    for r in [root, kernel] {
        assert_eq!(r.when(), When::Linux, "{}", r.name);
        assert_eq!(r.version(), Some(aterm_vz::pins::RELEASE));
    }
    let picked = |opts: &Options| -> Vec<&str> { select(opts).iter().map(|r| r.name).collect() };
    let linux = Options {
        linux: true,
        ..Options::default()
    };
    assert!(picked(&linux).contains(&"ubuntu-root") && picked(&linux).contains(&"ubuntu-kernel"));
    assert!(!picked(&Options::default()).contains(&"ubuntu-root"));
}

#[test]
fn a_release_ubuntu_archived_names_the_archive_host_and_the_bump() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let root = row("ubuntu-root").unwrap();
    let url = aterm_vz::pins::ROOT_ARCHIVE.url_at(aterm_vz::pins::RELEASE);
    let curl = "/usr/bin/curl exited Some(1): curl: (1) Protocol \"http\" disabled (in redirect)";
    let host = Fake::new().refuse(&url, curl);
    match provide(root, &m, &host, &mut |_| {}) {
        State::CouldNotRun(why) => {
            for needle in [
                "ubuntu-root 20260926:",
                "release-20260926",
                aterm_vz::pins::ARCHIVE_HOST,
                "Bump the image pin",
                curl,
            ] {
                assert!(why.contains(needle), "{needle}: {why}");
            }
        }
        other => panic!("{other:?}"),
    }
    let offline = "/usr/bin/curl exited Some(6): curl: (6) Could not resolve host";
    let host = Fake::new().refuse(&url, offline);
    match provide(root, &m, &host, &mut |_| {}) {
        State::Failed(why) => assert!(
            why.ends_with(offline) && !why.contains(aterm_vz::pins::ARCHIVE_HOST),
            "the negative control: another failure is curl's own: {why}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(names(&m.tooling), Vec::<String>::new());
}

/// A run whose only row Ubuntu archived is COULD NOT RUN in the archive's words, never a
/// red FAIL (§3). NEGATIVE CONTROL: a run whose fetch fails otherwise is FAIL.
#[test]
fn a_run_that_meets_an_archived_release_could_not_run() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let url = aterm_vz::pins::ROOT_ARCHIVE.url_at(aterm_vz::pins::RELEASE);
    let opts = Options {
        rows: vec!["ubuntu-root".into()],
        ..Options::default()
    };
    let curl = "/usr/bin/curl exited Some(1): curl: (1) Protocol \"http\" disabled (in redirect)";
    let mut said = Vec::new();
    let code = run(&opts, &m, &Fake::new().refuse(&url, curl), &mut |l| {
        said.push(l.to_string());
    });
    assert_eq!(code, crate::exit::COULD_NOT_RUN, "{said:#?}");
    assert!(
        said.iter()
            .any(|l| l.starts_with("COULD NOT RUN — ubuntu-root 20260926: Ubuntu has moved")),
        "{said:#?}"
    );
    assert_eq!(
        said.last().map(String::as_str),
        Some("COULD NOT RUN — 1 of 1 rows pin a file upstream no longer serves here")
    );

    let offline = "/usr/bin/curl exited Some(6): curl: (6) Could not resolve host";
    let mut said = Vec::new();
    let code = run(&opts, &m, &Fake::new().refuse(&url, offline), &mut |l| {
        said.push(l.to_string());
    });
    assert_eq!(code, crate::exit::FAILED, "{said:#?}");
}

/// A kernel row that lost its initrd, or holds one of another size, is not in place: a
/// check says so and a run fetches the row again. NEGATIVE CONTROL: the whole row reads in
/// place and fetches nothing.
#[test]
fn a_files_row_missing_a_file_is_not_in_place() {
    let home = aterm_tempfile::tempdir().unwrap();
    let m = machine(home.path());
    let at = |f| fixture_url(f).replace("{v}", aterm_vz::pins::RELEASE);
    let (vmlinuz, initrd) = (b"gzip kernel".to_vec(), b"an initrd".to_vec());
    let host = Fake::new()
        .serve(&at(aterm_vz::pins::VMLINUZ), vmlinuz.clone())
        .serve(&at(aterm_vz::pins::INITRD), initrd.clone());
    let row = kernel_row([(&vmlinuz, sha256(&vmlinuz)), (&initrd, sha256(&initrd))]);
    let State::Placed(exe) = provide(&row, &m, &host, &mut |_| {}) else {
        panic!("the row places")
    };
    assert_eq!(check(&row, &m, &host), State::Present(exe.clone()));
    let fetched = host.fetched.borrow().len();

    let lost = exe.with_file_name(aterm_vz::pins::INITRD.name());
    std::fs::remove_file(&lost).unwrap();
    match check(&row, &m, &host) {
        State::Missing(why) => assert!(
            why.contains(&format!(
                "lacks {} at its pinned size",
                aterm_vz::pins::INITRD.name()
            )),
            "{why}"
        ),
        other => panic!("a row without its initrd: {other:?}"),
    }
    assert_eq!(
        provide(&row, &m, &host, &mut |_| {}),
        State::Placed(exe.clone())
    );
    assert_eq!(host.fetched.borrow().len(), fetched + 2, "fetched again");
    assert_eq!(std::fs::read(&lost).unwrap(), initrd);

    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&lost, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::write(&lost, b"an initr").unwrap();
    assert!(matches!(check(&row, &m, &host), State::Missing(_)));
    assert_eq!(provide(&row, &m, &host, &mut |_| {}), State::Placed(exe));
    assert_eq!(std::fs::read(&lost).unwrap(), initrd);
}

/// The root image is held to being a regular file of its size: a member that is a link
/// (which `keep`'s check would follow) or of another size is refused, nothing placed.
#[test]
fn a_root_image_that_is_a_link_or_of_another_size_is_refused() {
    let ext4 = vec![0x53u8; 3000];
    let cases: [(&str, Vec<u8>, &str); 2] = [
        (
            "a link",
            gzip(&tar(&[
                ("real.img", Member::File(&ext4, 0o644)),
                (aterm_vz::pins::ROOT_MEMBER, Member::Symlink("real.img")),
            ])),
            "holds no image",
        ),
        (
            "another size",
            gzip(&tar(&[(
                aterm_vz::pins::ROOT_MEMBER,
                Member::File(&ext4[..2999], 0o644),
            )])),
            "of 2999 bytes, not the pinned image's 3000",
        ),
    ];
    let url = ROOT_URL.replace("{v}", aterm_vz::pins::RELEASE);
    for (what, archive, needle) in cases {
        let home = aterm_tempfile::tempdir().unwrap();
        let m = machine(home.path());
        let host = Fake::new().serve(&url, archive.clone());
        match provide(&root_row(&archive, 6000), &m, &host, &mut |_| {}) {
            State::Failed(why) => assert!(why.contains(needle), "{what}: {why}"),
            other => panic!("{what}: {other:?}"),
        }
        assert_eq!(names(&m.tooling), Vec::<String>::new(), "{what}");
    }
}

/// The lane holds the placed kernel to the stamp this row writes: the two digests are one.
#[test]
fn the_kernel_rows_stamp_is_the_one_the_lane_holds_it_to() {
    let kernel = row("ubuntu-kernel").unwrap();
    assert_eq!(
        files_digest(
            &kernel.files_for(aterm_vz::pins::PLATFORM),
            aterm_vz::pins::RELEASE
        ),
        aterm_vz::pins::kernel_stamp_sha256()
    );
}
