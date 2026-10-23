// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify provision` on a fake host in a scratch HOME.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::*;

/// One member of a fixture tar.
enum Member<'a> {
    Dir,
    File(&'a [u8], u32),
}

fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(text.as_bytes());
    field[digits] = 0;
}

/// A ustar archive of `members`.
fn tar(members: &[(&str, Member<'_>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, member) in members {
        let (kind, mode, body): (u8, u32, &[u8]) = match member {
            Member::Dir => (b'5', 0o755, &[]),
            Member::File(body, mode) => (b'0', *mode, body),
        };
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
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

/// `data` gzipped in stored DEFLATE blocks: no encoder between the fixture and the bytes
/// the extractor reads.
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

/// A thin 64-bit arm64 Mach-O loading `dylibs`, with `tail` after its load commands
/// (where the fake codesign reads a team).
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
/// What the fake codesign reads as another team's signature.
const OTHER_TEAM: &[u8] = b"TEAM=OTHERTEAM1";

fn sha256(bytes: &[u8]) -> String {
    let mut h = aterm_digest::Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// A row over one fixture archive served at `url`, pinned to `sha256` and `bytes`.
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

/// rustup holding everything the rustup row asks for: stable with every target and
/// clippy, the wasm32 pin with its target.
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

/// A machine with no rustup: rustup-init lays one, and every later command succeeds. Each
/// call is recorded with whether its program was executable when it ran.
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
    /// URL -> the fixture bytes it serves.
    served: BTreeMap<String, Vec<u8>>,
    fetched: RefCell<Vec<String>>,
    /// Where each download was written, and the cap it was handed.
    fetched_into: RefCell<Vec<(PathBuf, u64)>>,
    spawns: RefCell<Vec<Spawn>>,
    signed: RefCell<Vec<PathBuf>>,
    /// What the fake fish build lays at `<target>/release/fish`.
    built_fish: Vec<u8>,
    /// What the built fish answers to `--version`.
    fish_answers: String,
    fish_error: Option<&'static str>,
    fish_panics: bool,
    /// What a wasm-bindgen outside a scratch answers to `--version` (one this run builds,
    /// in its scratch, answers the pin).
    bindgen_answers: String,
    /// What the fake `ditto -x -k` lays in its tree: each file with its bytes (made
    /// executable), then each link with its target.
    zip_files: Vec<(&'static str, Vec<u8>)>,
    zip_links: Vec<(&'static str, PathBuf)>,
    rustup: Provisioned,
    /// A machine with no rustup, where one is asked for.
    bare: Option<Bare>,
}

impl Fake {
    fn new() -> Self {
        Self {
            served: BTreeMap::new(),
            fetched: RefCell::default(),
            spawns: RefCell::default(),
            signed: RefCell::default(),
            fetched_into: RefCell::default(),
            built_fish: macho(&[LIBSYSTEM], b""),
            fish_answers: "fish, version 4.9.3\n".into(),
            fish_error: None,
            fish_panics: false,
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

/// A scratch HOME's machine: its caches empty, rustup's homes holding a stub rustup.
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

/// Every name in `dir`, sorted (none where it does not exist).
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
        },
    )
}

/// §10 S7: a wrong sha256 refuses the row and leaves nothing: no install directory, no
/// scratch. NEGATIVE CONTROL: the same archive under its own sha256 is placed whole,
/// stamped and executable, and a size the archive does not have is refused first.
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
        },
    )
}

/// §10 S7: a Mach-O signed by another team refuses pwsh, and nothing is placed.
/// NEGATIVE CONTROL: the same tree with every Mach-O Microsoft's is placed, each Mach-O
/// checked, the modes set on unpack (pwsh 0755 from the archive's 0700) and the licence
/// and notices kept; without the notices it is refused.
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

/// A copy in place with no stamp, or a stamp of another sha256, is replaced by the pin's;
/// the old one goes with the scratch.
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
    assert_eq!(names(&m.tooling), ["pwsh-7.6.7"]);
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

/// §10 S7: a fish build whose binary links Homebrew's dynamic PCRE2 is refused by name,
/// and nothing is placed. NEGATIVE CONTROL: the same build linking only libSystem is
/// placed, its stamp recording the tarball's sha256, `fish --version` and the features.
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

/// A build that answers another version than the pin's (a tarball of another tag under
/// the pin's name) is refused, and nothing is placed. NEGATIVE CONTROL: the same build
/// answering the pin is placed (the PCRE2 test above).
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

/// §10 S7: the fish build's argv and environment are the row's, read from the spawn
/// record: `--no-default-features`, `FISH_BUILD_DOCS=0` and `PCRE2_SYS_STATIC=1` set, no
/// `RUSTFLAGS` set and every inherited one removed, a `*.noindex` target directory under
/// the tooling cache, run from outside the unpacked tree. The command it becomes drops
/// the inherited flags and `cargo run`'s words, and keeps the rest of the environment.
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

/// Lay every row a bare run wants as already in place on `m`: each install directory
/// stamped with its real pin, the browsers' too.
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

/// §10 S7: `--record` alone fetches wasm-bindgen. A bare run over a machine holding
/// every other row fetches and builds nothing; `--record` builds the CLI at the pin with
/// targo, `--locked`, and places it stamped. NEGATIVE CONTROL: neither a bare run nor
/// `--linux` selects it.
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

/// A wasm-bindgen already at the pin with no stamp (cargo's records and its `--version`
/// both the pin's, as the retired `ship provision` laid one) is rebuilt at the pin, never
/// adopted: nothing ties its bytes to crates.io, and nothing runs it. NEGATIVE CONTROL:
/// the copy this run builds is run, in its scratch, and placed stamped.
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

/// `--check` reads and changes nothing: a bare machine is every row missing, nothing is
/// fetched or created, and the exit is a failure.
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

/// A fixture browser pin served at a fixture URL, placed under `m`'s browser cache.
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
        sha256: Some(pin.sha256),
        platform: m.platform.clone(),
        features: None,
    };
    (pin, t)
}

/// A missing browser is fetched into a scratch under the tooling cache (where the reclaim
/// finds one a killed run left), unzipped by ditto, held to its binary, stamped for this
/// host and renamed into the lane's cache; a copy there without this pin's stamp is
/// replaced, never adopted. NEGATIVE CONTROL: bytes off the real pin are refused before
/// anything unzips.
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
    assert_eq!(names(&m.tooling), Vec::<String>::new(), "the scratch went");
    assert_eq!(names(&m.browser), ["chrome-headless-shell-0.0.1-mac-arm64"]);
}

/// A zip whose link leads outside its tree is refused, and nothing is placed; a zip that
/// lays its own `stamp.json` as a link into the tree gets a stamp of its own, written as
/// a new file, never through the link. NEGATIVE CONTROL: the program the link named keeps
/// its bytes, and the same zip without the escaping link is placed.
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

/// A host nothing is pinned for: every row says so, and the run is COULD NOT RUN.
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
            "pwsh",
            "binaryen",
            "chrome-headless-shell",
            "firefox",
            "webkit"
        ]
    );
}

/// Every pinned row's version, directory and program are its tool row's, so what this
/// verb places is what the resolver reads; each archive URL names that version, and a row
/// fetches for exactly the hosts its tool row's pin names, so a refusal names this verb
/// only where it provides the tool.
#[test]
fn each_row_places_what_its_tool_row_resolves() {
    for row in ROWS {
        let Some(tool) = row.tool_row() else {
            continue;
        };
        if let Some(pin) = tool.pin {
            for platform in ["macos-aarch64", "macos-x86_64"] {
                if let Some(a) = row.archive_for(platform) {
                    assert!(a.url_at(pin.version).contains(pin.version), "{}", a.url);
                    assert_eq!(a.sha256.len(), 64);
                }
            }
            match row.install {
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

/// rustup-init is fetched at its pin into a scratch of its own and held to its size and
/// sha256 before it runs: off its pin it never runs, and nothing is left. NEGATIVE
/// CONTROL: the same bytes under their own sha256 run from that scratch, executable, and
/// the scratch goes with the run.
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

/// The real rustup row pins rustup-init for each host it runs on, each URL naming the
/// version.
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

/// A tree without `publish/` (the public snapshot, which excludes it) wants no wasm32 pin:
/// the rustup row is in place on stable alone, and a run passes. NEGATIVE CONTROL: a tree
/// whose `publish/config.sh` has no wasm32 clause is told so, and its run fails.
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

/// A stamp is this pin's only for this host and this build: one placed for another host,
/// one naming other features, one the retired adoption wrote and one of another version
/// are each missing to `--check` and replaced by a run. NEGATIVE CONTROL: the stamp a run
/// writes is in place.
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

/// A run that finds a row in place marks its stamp used, which the reclaim's idle rule
/// reads as use. NEGATIVE CONTROL: `--check` finds it in place and leaves the stamp as it
/// was.
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

/// The download reads no `~/.curlrc` (`-q` first, as curl requires), keeps to https
/// through every redirect, and stops once the body passes the pin's size.
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

/// A Mac's tools are the machine's: an x86_64 build under Rosetta provisions the arm64
/// rows a native run placed, and nothing else changes the host.
#[test]
fn a_translated_run_provisions_the_macs_own_rows() {
    assert_eq!(platform_of("macos", "x86_64", true), "macos-aarch64");
    assert_eq!(platform_of("macos", "x86_64", false), "macos-x86_64");
    assert_eq!(platform_of("macos", "aarch64", false), "macos-aarch64");
    assert_eq!(platform_of("linux", "x86_64", true), "linux-x86_64");
}
