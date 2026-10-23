// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Source-build cargo gate (`find_cargo_and_rustup`, `cargo_honours_pin`, `cargo_build_pinned`) tests.

use std::path::{Path, PathBuf};

use crate::support::{
    Home, Lib, Rec, Scratch, chmod, install_sh, installer, q, read, root, s, symlink, system_path,
    tree, write,
};

/// The toolchain the fake rustup resolves the pin to, and a second one.
const PIN: &str = "trust-x86_64-apple-darwin";
const FOREIGN: &str = "foreign-8.8.8-x86_64-apple-darwin";
const PIN_VERSION: &str = "cargo 9.9.9 (fake00000 2026-01-01)";
const FOREIGN_VERSION: &str = "cargo 8.8.8-dev (foreign00 2026-02-02)";
const SHADOW_VERSION: &str = "cargo 1.95.0 (c1ab5a1c3 2026-04-02) (Homebrew)";
const STORE_TARGO_VERSION: &str = "targo 9.9.9-dev (fake00000 2026-01-01) (targo 0.1.0)";
/// The one `--config` that names rustup's lane unverified.
const UNVERIFIED_LANE: &str = "target.'cfg(trust_verify)'.rustflags=[\"-Ztrust-verify=off\"]";

/// rustup, faked: dispatched by its name; RUSTUP_TOOLCHAIN, then a directory
/// override on the cwd (FAKE_DIR_OVERRIDE), then the cwd's rust-toolchain.toml
/// named as getcwd() spells it.
const RUSTUP: &str = r#"#!/bin/sh
pin="${FAKE_TOOLCHAIN_NAME:?}"
foreign="${FAKE_FOREIGN_NAME:?}"
here="$(env pwd -P)"
if [ -n "${RUSTUP_TOOLCHAIN:-}" ]; then
	tc="$RUSTUP_TOOLCHAIN"
	why="overridden by environment variable RUSTUP_TOOLCHAIN"
elif [ -n "${FAKE_DIR_OVERRIDE:-}" ]; then
	tc="$FAKE_DIR_OVERRIDE"
	why="directory override for '$here'"
else
	tc="$pin"
	why="overridden by '$here/rust-toolchain.toml'"
fi
installed() {
	if [ "$1" = "$pin" ]; then [ -z "${FAKE_TOOLCHAIN_MISSING:-}" ]; else [ "$1" = "$foreign" ]; fi
}
own_cargo() {
	if [ "$1" = "$pin" ]; then echo "${FAKE_TOOLCHAIN_CARGO:?}"; else echo "${FAKE_FOREIGN_CARGO:?}"; fi
}
no_download() {
	[ "${RUSTUP_AUTO_INSTALL:-}" = 0 ] || {
		echo "fake rustup: a probe without RUSTUP_AUTO_INSTALL=0 would download a missing toolchain: $(basename "$0") $*" >&2
		exit 3
	}
}
need_toolchain() {
	installed "$tc" || {
		echo "error: toolchain '$tc' is not installed" >&2
		exit 1
	}
}
case "$(basename "$0")" in
rustup)
	no_download "$@"
	case "$*" in
	"which cargo") need_toolchain; own_cargo "$tc" ;;
	"show active-toolchain") need_toolchain; echo "$tc ($why)" ;;
	"toolchain list")
		for t in "$pin" "$foreign"; do
			installed "$t" || continue
			if [ "$t" = "$tc" ]; then echo "$t (active)"; else echo "$t"; fi
		done
		;;
	*) echo "fake rustup: unexpected: $*" >&2; exit 2 ;;
	esac
	;;
cargo)
	case "${1:-}" in
	--version | -V) no_download "$@"; need_toolchain; "$(own_cargo "$tc")" --version ;;
	*)
		ch="${CARGO_HOME:-$HOME/.cargo}/bin"
		case ":$PATH:" in
		*":$ch:"*) ;;
		*) PATH="$ch:$PATH" ;;
		esac
		echo "proxy[$tc] $* (rustc: $(command -v rustc || echo none))" >>"${SHIM_LOG:?}"
		exit 97
		;;
	esac
	;;
rustc) no_download "$@"; need_toolchain; echo "rustc 9.9.9 (fake00000 2026-01-01)" ;;
*) echo "fake rustup: invoked as $(basename "$0")" >&2; exit 2 ;;
esac
"#;

/// Homebrew's cargo: its own version, whatever RUSTUP_TOOLCHAIN says.
const SHADOW_CARGO: &str = r#"#!/bin/sh
case "${1:-}" in
--version | -V) echo "cargo 1.95.0 (c1ab5a1c3 2026-04-02) (Homebrew)" ;;
*) echo "shadow $*" >>"${SHIM_LOG:?}"; exit 97 ;;
esac
"#;

const SHADOW_RUSTC: &str = "#!/bin/sh\necho \"rustc 1.95.0 (c1ab5a1c3 2026-04-02) (Homebrew)\"\n";

/// A plain cargo built from the pin's commit: the pin's version line, and no
/// idea what RUSTUP_TOOLCHAIN is.
const TWIN_CARGO: &str = r#"#!/bin/sh
case "${1:-}" in
--version | -V) echo "cargo 9.9.9 (fake00000 2026-01-01)" ;;
*) echo "twin $*" >>"${SHIM_LOG:?}"; exit 97 ;;
esac
"#;

/// The store's targo: `--unverified build …` is the one build shape it takes,
/// recorded with the first PATH dir and any ambient compiler steer; a plain
/// `build` is refused as targo refuses it.
const STORE_TARGO: &str = r#"#!/bin/sh
case "${1:-}" in
--version | -V) echo "targo 9.9.9-dev (fake00000 2026-01-01) (targo 0.1.0)" ;;
--unverified)
	shift
	[ -z "${RUSTC:-}${CARGO_BUILD_RUSTC:-}" ] || echo "targo[9192] STEERED: RUSTC=${RUSTC:-} CARGO_BUILD_RUSTC=${CARGO_BUILD_RUSTC:-}" >>"${SHIM_LOG:?}"
	echo "targo[9192] --unverified $* (PATH first: ${PATH%%:*})" >>"${SHIM_LOG:?}"
	exit 97
	;;
*)
	echo "targo[9192] REFUSED the implicit lane: $*" >>"${SHIM_LOG:?}"
	echo "error: \`targo build\` refuses to create an implicitly unverified artifact." >&2
	exit 1
	;;
esac
"#;

/// `aterm pkg which` for the store at `$FAKE_ATPKG`, in atpkg's printed
/// shapes: the managed line, or SHADOWED when another targo precedes the
/// managed bin/ on PATH; `alab-targo` names the managed copy unless
/// FAKE_NO_ALIAS says the alias is not laid.
const FAKE_ATERM: &str = r#"#!/bin/sh
managed_bin="${FAKE_ATPKG:?}/bin"
store_bin="$FAKE_ATPKG/store/trust/9192/bin"
first=""
old_ifs=$IFS
IFS=:
for d in $PATH; do
	[ -n "$d" ] && [ -x "$d/targo" ] || continue
	first="$d"
	break
done
IFS=$old_ifs
case "$*" in
"pkg which targo")
	if [ -n "$first" ] && [ "$(cd "$first" && pwd -P)" != "$managed_bin" ]; then
		echo "targo → $first/targo — managed 9192 — SHADOWED by $first/targo — type alab-targo for the managed one"
	else
		echo "targo → $managed_bin/targo → $store_bin/targo — managed 9192 — pinned by index 39"
	fi
	;;
"pkg which alab-targo")
	if [ -n "${FAKE_NO_ALIAS:-}" ]; then
		echo "atpkg: alab-targo is not installed (fix: aterm pkg install alab-targo)" >&2
		exit 1
	fi
	echo "alab-targo → $managed_bin/alab-targo → $store_bin/targo — managed 9192 — pinned by index 39"
	;;
*) echo "fake aterm: unexpected: $*" >&2; exit 2 ;;
esac
"#;

/// curl and gh: no run here may reach the network; any call is recorded.
const NO_NETWORK: &str =
    "#!/bin/sh\necho \"network: $(basename \"$0\") $*\" >>\"${SHIM_LOG:?}\"\nexit 1\n";

/// `uname` for the OS whose remedies a run names (`-s` and bare), the
/// machine's own for anything else: Trust is published for macOS only, so the
/// remedies differ by OS, and every lab run names the OS it means rather than
/// inheriting the host's.
const DARWIN_UNAME: &str =
    "#!/bin/sh\ncase \"${1:--s}\" in -s) echo Darwin ;; *) exec /usr/bin/uname \"$@\" ;; esac\n";
const LINUX_UNAME: &str =
    "#!/bin/sh\ncase \"${1:--s}\" in -s) echo Linux ;; *) exec /usr/bin/uname \"$@\" ;; esac\n";

/// An `aterm` with no Trust store.
const BARE_ATERM: &str = "#!/bin/sh\necho \"atpkg: ${3:-it} is not installed (fix: aterm pkg install ${3:-it})\" >&2\nexit 1\n";

/// The remedy commands a refusal can name.
const REMEDIES: &[&str] = &[
    "aterm pkg install trust",
    "aterm pkg doctor",
    "https://rustup.rs",
    "rustup toolchain link trust <prefix>",
    "rustup toolchain install",
    "rustup override unset",
    "export PATH=",
    "brew uninstall rust",
];

fn remedies(reason: &str) -> Vec<&'static str> {
    REMEDIES
        .iter()
        .copied()
        .filter(|r| reason.contains(r))
        .collect()
}

/// Whether `reason` names each of `names`, in this order, and none of `lacks`.
fn says(reason: &str, names: &[impl AsRef<str>], lacks: &[impl AsRef<str>]) -> bool {
    let mut from = 0;
    names.iter().all(|name| {
        let name = name.as_ref();
        reason[from..].find(name).is_some_and(|at| {
            from += at + name.len();
            true
        })
    }) && !lacks.iter().any(|lack| reason.contains(lack.as_ref()))
}

/// The fixtures, laid under one scratch dir (spelled physically, as the gate
/// reports every path).
struct Lab {
    t: PathBuf,
    /// A checkout pinning `trust`.
    checkout: PathBuf,
    log: PathBuf,
}

impl Lab {
    fn new(scratch: &Scratch) -> Self {
        let t = scratch.mkdir("lab");
        let lay = |rel: &str, body: &str| scratch.stand_in(&format!("lab/{rel}"), body);
        for dir in [
            "cargo-home/bin",
            "rustup-keg",
            "rustc-less",
            "plain-rustc",
            "bare-rustup",
            "impostor",
        ] {
            lay(&format!("{dir}/rustup"), RUSTUP);
        }
        lay("rustc-home/bin/rustc", RUSTUP);
        for dir in ["cargo-home/bin", "rustup-keg", "rustc-less", "plain-rustc"] {
            symlink("rustup", &t.join(dir).join("cargo"));
        }
        for dir in ["cargo-home/bin", "rustup-keg"] {
            symlink("rustup", &t.join(dir).join("rustc"));
        }
        lay("shadow/cargo", SHADOW_CARGO);
        lay("shadow/rustc", SHADOW_RUSTC);
        lay("impostor/cargo", SHADOW_CARGO);
        lay("plain-rustc/rustc", SHADOW_RUSTC);
        lay("twin/cargo", TWIN_CARGO);
        lay(
            "toolchain/bin/cargo",
            &format!("#!/bin/sh\necho \"{PIN_VERSION}\"\n"),
        );
        lay(
            "foreign/bin/cargo",
            &format!("#!/bin/sh\necho \"{FOREIGN_VERSION}\"\n"),
        );
        lay("nonet/curl", NO_NETWORK);
        lay("nonet/gh", NO_NETWORK);
        lay("macos/uname", DARWIN_UNAME);
        lay("linux/uname", LINUX_UNAME);
        for dir in ["nohome", "none"] {
            scratch.mkdir(&format!("lab/{dir}"));
        }
        lay("atpkg/store/trust/9192/bin/targo", STORE_TARGO);
        lay(
            "atpkg/store/trust/9192/bin/trustc",
            "#!/bin/sh\necho \"rustc 9.9.9-dev (fake00000 2026-01-01) (trustc 0.1.0)\"\n",
        );
        symlink("9192", &t.join("atpkg/store/trust/current"));
        let shim = |store: &str| format!("#!/bin/sh\nexec '{}' \"$@\"\n", s(&t.join(store)));
        scratch.exe(
            "lab/atpkg/bin/targo",
            &shim("atpkg/store/trust/9192/bin/targo"),
        );
        scratch.exe(
            "lab/atpkg/bin/alab-targo",
            &shim("atpkg/store/trust/9192/bin/targo"),
        );
        lay("atpkg-cli/aterm", FAKE_ATERM);
        lay("bare-aterm/aterm", BARE_ATERM);
        lay("atpkg-broken/store/trust/9193/bin/targo", STORE_TARGO);
        scratch.exe(
            "lab/atpkg-broken/bin/targo",
            &shim("atpkg-broken/store/trust/9193/bin/targo"),
        );
        lay("stray-targo/targo", STORE_TARGO);
        scratch.write(
            "lab/stock-pin/rust-toolchain.toml",
            "[toolchain]\nchannel = \"stable\"\n",
        );
        let checkout = scratch.mkdir("lab/checkout");
        write(
            &checkout.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"trust\"\n",
        );
        let log = t.join("builds.log");
        write(&log, "");
        Self { t, checkout, log }
    }

    /// A fixture dir (or file) by its name under the lab.
    fn p(&self, rel: &str) -> String {
        s(&self.t.join(rel)).into()
    }

    /// `dirs` (lab names), macOS's `uname` (a row names `linux` among its
    /// dirs to be a Linux run), the network guard, then the system dirs, as a
    /// PATH.
    fn path(&self, dirs: &[&str]) -> String {
        let mut parts: Vec<String> = dirs.iter().map(|d| self.p(d)).collect();
        parts.push(self.p("macos"));
        parts.push(self.p("nonet"));
        parts.push(system_path().into());
        parts.join(":")
    }

    fn store_bin(&self) -> String {
        self.p("atpkg/store/trust/9192/bin")
    }

    /// What the stand-ins read.
    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("SHIM_LOG", s(&self.log).into()),
            ("FAKE_TOOLCHAIN_NAME", PIN.into()),
            ("FAKE_TOOLCHAIN_CARGO", self.p("toolchain/bin/cargo")),
            ("FAKE_FOREIGN_NAME", FOREIGN.into()),
            ("FAKE_FOREIGN_CARGO", self.p("foreign/bin/cargo")),
            ("FAKE_ATPKG", self.p("atpkg")),
        ]
    }

    /// A library batch with the stand-ins' environment exported.
    fn lib(&self) -> Lib {
        let mut lib = Lib::new();
        for (key, value) in self.env() {
            lib.env(key, &value);
        }
        lib
    }

    /// The recorded builds, emptied.
    fn take_builds(&self) -> String {
        let builds = read(&self.log);
        write(&self.log, "");
        builds
    }
}

/// What `find_cargo_and_rustup` decided.
#[derive(Debug)]
struct Found {
    ok: bool,
    lane: String,
    cargo: String,
    rustup: String,
    reason: String,
    prefix: String,
    build: String,
    decline: String,
}

const FOUND: &[&str] = &[
    "CARGO_LANE",
    "CARGO_BIN",
    "RUSTUP_BIN",
    "CARGO_RESOLVE_REASON",
    "MANAGED_PREFIX",
    "MANAGED_BUILD",
    "MANAGED_DECLINE",
];

impl From<&Rec> for Found {
    fn from(rec: &Rec) -> Self {
        Self {
            ok: rec.rc == 0,
            lane: rec.var("CARGO_LANE").into(),
            cargo: rec.var("CARGO_BIN").into(),
            rustup: rec.var("RUSTUP_BIN").into(),
            reason: rec.var("CARGO_RESOLVE_REASON").into(),
            prefix: rec.var("MANAGED_PREFIX").into(),
            build: rec.var("MANAGED_BUILD").into(),
            decline: rec.var("MANAGED_DECLINE").into(),
        }
    }
}

/// What `cargo_honours_pin` proved.
#[derive(Debug)]
struct Proved {
    ok: bool,
    version: String,
    toolchain: String,
    rustc: String,
    reason: String,
}

const PROVED: &[&str] = &[
    "CARGO_VERSION",
    "CARGO_TOOLCHAIN",
    "CARGO_RUSTC",
    "CARGO_RESOLVE_REASON",
];

impl From<&Rec> for Proved {
    fn from(rec: &Rec) -> Self {
        Self {
            ok: rec.rc == 0,
            version: rec.var("CARGO_VERSION").into(),
            toolchain: rec.var("CARGO_TOOLCHAIN").into(),
            rustc: rec.var("CARGO_RUSTC").into(),
            reason: rec.var("CARGO_RESOLVE_REASON").into(),
        }
    }
}

/// One gate run in the batch: `setup` (shell, e.g. exports), then the lane
/// search under `path`/`home` against `root` (empty: no root), then the proof
/// when asked; `teardown` after.
struct Gate {
    find: usize,
    proof: Option<usize>,
}

fn gate(
    lib: &mut Lib,
    lab: &Lab,
    path: &str,
    home: &str,
    root: &str,
    setup: &str,
    prove: bool,
) -> Gate {
    lib.step(format!(
        "export PATH={} CARGO_HOME={}",
        q(path),
        q(&lab.p(home))
    ));
    lib.step(setup);
    let find = lib.call(format!("find_cargo_and_rustup {}", q(root)), FOUND);
    let proof = prove.then(|| lib.call(format!("cargo_honours_pin {}", q(root)), PROVED));
    lib.step(format!(
        "unset FAKE_TOOLCHAIN_MISSING FAKE_DIR_OVERRIDE FAKE_NO_ALIAS RUSTUP_TOOLCHAIN; export PATH={}",
        q(system_path())
    ));
    Gate { find, proof }
}

/// `path` with every ASCII letter's case swapped, when that spelling reaches
/// the same file (a case-insensitive volume, macOS's default); else `None`.
fn case_changed(path: &Path) -> Option<PathBuf> {
    let flipped: String = s(path)
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let flipped = PathBuf::from(flipped);
    let same = |a: &Path, b: &Path| {
        use std::os::unix::fs::MetadataExt as _;
        let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
            return false;
        };
        (a.dev(), a.ino()) == (b.dev(), b.ino())
    };
    (flipped != path && same(&flipped, path)).then_some(flipped)
}

/// rustup's dist channels — what one `rustup toolchain install` fetches —
/// against the custom names only `rustup toolchain link` makes.
#[test]
fn rustup_dist_channels_follow_rustups_grammar() {
    let scratch = Scratch::new();
    let dist = [
        "stable",
        "beta",
        "nightly",
        "1.97",
        "1.97.1",
        "nightly-2025-11-13",
        "1.97.0-beta",
        "1.97.0-beta.1",
        "beta-2026-01-01",
        "1.97.1-x86_64-apple-darwin",
        "stable-x86_64-unknown-linux-gnu",
        "nightly-2025-11-13-x86_64-apple-darwin",
    ];
    let custom = [
        "trust",
        "trust-x86_64-apple-darwin",
        "stage2",
        "1",
        "my-1.97.1",
        "stable_x",
        "",
    ];
    let mut lib = Lib::new();
    let calls: Vec<(&str, bool, usize)> = dist
        .iter()
        .map(|n| (*n, true))
        .chain(custom.iter().map(|n| (*n, false)))
        .map(|(name, want)| {
            (
                name,
                want,
                lib.call(format!("rustup_dist_channel {}", q(name)), &[]),
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    for (name, want, i) in calls {
        assert_eq!(recs[i].rc == 0, want, "{name:?}");
    }
}

/// One rustup-lane layout: the PATH (lab dirs), `$CARGO_HOME`, the root the
/// gate is given and any setup; and the cargo, rustup and rustc it must prove.
struct Layout<'a> {
    dirs: &'a [&'a str],
    home: &'a str,
    root: String,
    setup: &'a str,
    proved: [&'a str; 3],
}

/// The proxy wins over a Homebrew cargo first on PATH — at `$CARGO_HOME`, on
/// PATH behind the shadow, beside a keg rustup, or split from its rustc — and
/// the proof names the pin file, the pin's version and the rustc the build
/// will take.
#[test]
fn the_rustup_lane_takes_the_proxy_and_proves_the_pin() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let root = s(&lab.checkout).to_string();
    let linked = scratch.join("lab/checkout-link");
    symlink(&lab.checkout, &linked);
    let flipped = case_changed(&lab.checkout);
    let home = [
        "cargo-home/bin/cargo",
        "cargo-home/bin/rustup",
        "cargo-home/bin/rustc",
    ];
    let at = |dirs, home: &'static str, root: &str, setup, proved| Layout {
        dirs,
        home,
        root: root.into(),
        setup,
        proved,
    };
    let mut rows = vec![
        at(&["shadow"], "cargo-home", &root, "", home),
        at(&["shadow", "cargo-home/bin"], "cargo-home", &root, "", home),
        at(
            &["shadow", "rustup-keg"],
            "nohome",
            &root,
            "",
            ["rustup-keg/cargo", "rustup-keg/rustup", "rustup-keg/rustc"],
        ),
        at(
            &["shadow", "rustc-home/bin", "rustc-less"],
            "rustc-home",
            &root,
            "",
            [
                "rustc-less/cargo",
                "rustc-less/rustup",
                "rustc-home/bin/rustc",
            ],
        ),
        at(&["shadow"], "cargo-home", s(&linked), "", home),
        at(
            &["shadow"],
            "cargo-home",
            &root,
            "export RUSTUP_TOOLCHAIN=foreign-8.8.8-x86_64-apple-darwin",
            home,
        ),
    ];
    if let Some(flipped) = &flipped {
        rows.push(at(&["shadow"], "cargo-home", s(flipped), "", home));
    } else {
        eprintln!(
            "NOTE: {root} is on a case-sensitive volume: the case-changed checkout is not exercised"
        );
    }
    let mut lib = lab.lib();
    let gates: Vec<Gate> = rows
        .iter()
        .map(|r| {
            gate(
                &mut lib,
                &lab,
                &lab.path(r.dirs),
                r.home,
                &r.root,
                r.setup,
                true,
            )
        })
        .collect();
    let recs = lib.run(&scratch);
    let pin_file = format!("{PIN} (overridden by '{root}/rust-toolchain.toml')");
    for (row, g) in rows.iter().zip(&gates) {
        let label = format!(
            "PATH {:?} CARGO_HOME {} root {} {}",
            row.dirs, row.home, row.root, row.setup
        );
        let found = Found::from(&recs[g.find]);
        let proved = Proved::from(&recs[g.proof.expect("proved")]);
        let [cargo, rustup, rustc] = row.proved.map(|rel| lab.p(rel));
        assert!(found.ok, "{label}: {}", found.reason);
        assert_eq!(
            (found.lane.as_str(), found.cargo, found.rustup),
            ("rustup", cargo, rustup),
            "{label}"
        );
        assert!(proved.ok, "{label}: {}", proved.reason);
        assert_eq!(
            (
                proved.version.as_str(),
                proved.toolchain.as_str(),
                proved.rustc
            ),
            (PIN_VERSION, pin_file.as_str(), rustc),
            "{label}"
        );
    }
    assert_eq!(lab.take_builds(), "", "the pre-flight never builds");
}

/// Each cargo that cannot honour the pin is refused, and the refusal names the
/// remedy for its case: the dir beside rustup to put first, the Homebrew way
/// out, rustup's own resolution, the pinned toolchain's own cargo or rustc.
#[test]
fn a_cargo_that_cannot_honour_the_pin_is_refused_naming_its_remedy() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let root = s(&lab.checkout).to_string();
    let bare = lab.p("bare-rustup");
    let prepend = format!("export PATH=\"{bare}:$PATH\"");
    struct Row<'a> {
        label: &'a str,
        dirs: Vec<&'a str>,
        home: &'a str,
        setup: &'a str,
        /// The candidate the search takes; `None`: refused by the search.
        cargo: Option<&'a str>,
        /// What the refusal must name, in order, and must not.
        names: Vec<String>,
        lacks: Vec<String>,
    }
    let rows = vec![
        Row {
            label: "a plain cargo, no rustup",
            dirs: vec!["shadow"],
            home: "nohome",
            setup: "",
            cargo: None,
            names: vec![
                SHADOW_VERSION.into(),
                "https://rustup.rs".into(),
                "rustup toolchain link trust <prefix>".into(),
            ],
            lacks: vec![
                "aterm pkg install trust".into(),
                "brew uninstall rust".into(),
            ],
        },
        Row {
            label: "a plain cargo, no rustup, aterm in reach",
            dirs: vec!["shadow", "bare-aterm"],
            home: "nohome",
            setup: "",
            cargo: None,
            names: vec!["aterm pkg install trust".into()],
            lacks: vec!["https://rustup.rs".into(), "brew uninstall rust".into()],
        },
        Row {
            label: "the shadow beside a rustup with no proxy",
            dirs: vec!["shadow", "bare-rustup"],
            home: "nohome",
            setup: "",
            cargo: Some("shadow/cargo"),
            names: vec![
                SHADOW_VERSION.into(),
                format!("{} ({PIN_VERSION})", lab.p("toolchain/bin/cargo")),
                prepend.clone(),
                "brew uninstall rust".into(),
            ],
            lacks: vec![lab.p("nohome")],
        },
        Row {
            label: "the twin: the pin's version line from a plain cargo",
            dirs: vec!["twin", "bare-rustup"],
            home: "nohome",
            setup: "",
            cargo: Some("twin/cargo"),
            names: vec![
                format!("{} ({PIN_VERSION})", lab.p("twin/cargo")),
                prepend.clone(),
            ],
            lacks: vec![lab.p("nohome")],
        },
        Row {
            label: "a cargo beside rustup that is not its proxy",
            dirs: vec!["impostor"],
            home: "nohome",
            setup: "",
            cargo: Some("impostor/cargo"),
            names: vec![lab.p("impostor/rustup"), "https://rustup.rs".into()],
            lacks: vec!["export PATH=".into()],
        },
        Row {
            label: "a proxy pair with no rustc, behind the shadow",
            dirs: vec!["shadow", "rustc-less"],
            home: "nohome",
            setup: "",
            cargo: Some("rustc-less/cargo"),
            names: vec![lab.p("toolchain/bin/rustc"), "https://rustup.rs".into()],
            lacks: Vec::new(),
        },
        Row {
            label: "a proxy pair beside a plain rustc",
            dirs: vec!["plain-rustc"],
            home: "nohome",
            setup: "",
            cargo: Some("plain-rustc/cargo"),
            names: vec![lab.p("plain-rustc/rustc")],
            lacks: Vec::new(),
        },
        Row {
            label: "the pinned toolchain absent",
            dirs: vec!["shadow"],
            home: "cargo-home",
            setup: "export FAKE_TOOLCHAIN_MISSING=1",
            cargo: Some("cargo-home/bin/cargo"),
            names: vec![
                format!("toolchain '{PIN}' is not installed"),
                "aterm pkg install trust".into(),
                "rustup toolchain link trust <prefix>".into(),
            ],
            lacks: Vec::new(),
        },
        Row {
            label: "the pinned toolchain absent, on Linux",
            dirs: vec!["linux", "shadow"],
            home: "cargo-home",
            setup: "export FAKE_TOOLCHAIN_MISSING=1",
            cargo: Some("cargo-home/bin/cargo"),
            names: vec![
                format!("toolchain '{PIN}' is not installed"),
                "build the Trust toolchain from source (https://github.com/alabsystems/trust \
                 publishes no build for Linux)"
                    .into(),
                "rustup toolchain link trust <prefix>".into(),
            ],
            lacks: vec!["aterm pkg install trust".into(), "tarballs".into()],
        },
        Row {
            label: "a rustup override on the checkout",
            dirs: vec!["shadow"],
            home: "cargo-home",
            setup: "export FAKE_DIR_OVERRIDE=foreign-8.8.8-x86_64-apple-darwin",
            cargo: Some("cargo-home/bin/cargo"),
            names: vec![
                format!("{FOREIGN} (directory override for '{root}')"),
                format!("rustup override unset --path {root}"),
            ],
            lacks: Vec::new(),
        },
    ];
    let mut lib = lab.lib();
    let gates: Vec<Gate> = rows
        .iter()
        .map(|r| {
            gate(
                &mut lib,
                &lab,
                &lab.path(&r.dirs),
                r.home,
                &root,
                r.setup,
                r.cargo.is_some(),
            )
        })
        .collect();
    let flipped = case_changed(&lab.checkout).map(|flipped| {
        gate(
            &mut lib,
            &lab,
            &lab.path(&["shadow"]),
            "cargo-home",
            s(&flipped),
            "export FAKE_DIR_OVERRIDE=foreign-8.8.8-x86_64-apple-darwin",
            true,
        )
    });
    let recs = lib.run(&scratch);
    for (row, g) in rows.iter().zip(&gates) {
        let found = Found::from(&recs[g.find]);
        let reason = match (row.cargo, g.proof) {
            (None, _) => {
                assert!(!found.ok, "{}: refused by the search", row.label);
                found.reason
            }
            (Some(cargo), Some(proof)) => {
                assert!(found.ok, "{}: {}", row.label, found.reason);
                assert_eq!(found.cargo, lab.p(cargo), "{}", row.label);
                let proved = Proved::from(&recs[proof]);
                assert!(!proved.ok, "{}: refused by the proof", row.label);
                assert_eq!(
                    proved.rustc, "",
                    "{}: a refused rustc is not the build's",
                    row.label
                );
                proved.reason
            }
            (Some(_), None) => unreachable!(),
        };
        assert!(
            says(&reason, &row.names, &row.lacks),
            "{}: names {:?} in order and none of {:?}: {reason}",
            row.label,
            row.names,
            row.lacks
        );
    }
    if let Some(g) = flipped {
        let proved = Proved::from(&recs[g.proof.expect("proved")]);
        assert!(!proved.ok);
        assert!(
            proved
                .reason
                .ends_with(&format!("rustup override unset --path {root}")),
            "{}",
            proved.reason
        );
    }
    assert_eq!(lab.take_builds(), "", "the pre-flight never builds");
}

/// The build runs under exactly what the proof chose: rustup's cargo from the
/// checkout, the unverified lane named, RUSTUP_TOOLCHAIN scrubbed, and the
/// proved rustc first on PATH for that one command.
#[test]
fn a_build_runs_under_exactly_what_the_proof_chose() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let root = s(&lab.checkout).to_string();
    let ambient = "export RUSTUP_TOOLCHAIN=foreign-8.8.8-x86_64-apple-darwin";
    let rows: [(&[&str], &str, &str, &str); 4] = [
        (
            &["shadow", "cargo-home/bin"],
            "cargo-home",
            "",
            "cargo-home/bin/rustc",
        ),
        (&["shadow", "rustup-keg"], "nohome", "", "rustup-keg/rustc"),
        (
            &["shadow", "rustc-home/bin", "rustc-less"],
            "rustc-home",
            "",
            "rustc-home/bin/rustc",
        ),
        (&["shadow"], "cargo-home", ambient, "cargo-home/bin/rustc"),
    ];
    let mut lib = lab.lib();
    let mut calls = Vec::new();
    for (dirs, home, setup, _) in &rows {
        let path = lab.path(dirs);
        lib.step(format!(
            "export PATH={} CARGO_HOME={}",
            q(&path),
            q(&lab.p(home))
        ));
        lib.step(*setup);
        lib.step(format!(
            "find_cargo_and_rustup {r} && cargo_honours_pin {r}",
            r = q(&root)
        ));
        let control = lib.call(
            format!(
                "(cd {r} && \"$CARGO_BIN\" build --release -p aterm --target-dir {r}/target)",
                r = q(&root)
            ),
            &[],
        );
        let control_log = lib.call("cat \"$SHIM_LOG\"; : >\"$SHIM_LOG\"", &[]);
        let build = lib.call(
            format!("cargo_build_pinned {}", q(&root)),
            &["PATH", "RUSTUP_TOOLCHAIN"],
        );
        let build_log = lib.call("cat \"$SHIM_LOG\"; : >\"$SHIM_LOG\"", &[]);
        lib.step("unset RUSTUP_TOOLCHAIN");
        calls.push((path, control, control_log, build, build_log));
    }
    let recs = lib.run(&scratch);
    for ((dirs, _, setup, rustc), (path, control, control_log, build, build_log)) in
        rows.iter().zip(calls)
    {
        let label = format!("{dirs:?} {setup}");
        assert_eq!(recs[control].rc, 97, "{label}");
        let control = &recs[control_log].out;
        if *setup == ambient {
            assert!(
                control.starts_with(&format!("proxy[{FOREIGN}]")),
                "{label}: the control runs under the ambient toolchain: {control}"
            );
        } else {
            assert!(
                control.ends_with(&format!("(rustc: {})", lab.p("shadow/rustc"))),
                "{label}: the control takes the shadow's rustc: {control}"
            );
        }
        assert_eq!(recs[build].rc, 97, "{label}: the build's own exit status");
        assert_eq!(
            recs[build_log].out,
            format!(
                "proxy[{PIN}] build --release -p aterm --target-dir {root}/target --config {UNVERIFIED_LANE} (rustc: {})",
                lab.p(rustc)
            ),
            "{label}"
        );
        assert_eq!(
            recs[build].var("PATH"),
            path,
            "{label}: the caller's PATH is untouched"
        );
        if *setup == ambient {
            assert_eq!(
                recs[build].var("RUSTUP_TOOLCHAIN"),
                FOREIGN,
                "{label}: the caller's RUSTUP_TOOLCHAIN is untouched"
            );
        }
    }
}

/// The aterm-managed store comes first — answered by `aterm pkg which`, a
/// shim on PATH, or a store bin dir reached through `current` — over a
/// Homebrew cargo first on PATH and over rustup's proxies; it is proved by
/// targo's own line and the trustc beside it, and builds on targo's explicit
/// unverified lane with the store's bin dir first and no ambient compiler
/// steer.
#[test]
fn the_managed_store_lane_comes_first_and_builds_on_targos_explicit_lane() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let root = s(&lab.checkout).to_string();
    let store_bin = lab.store_bin();
    let toolchain = format!(
        "trust (pinned by '{root}/rust-toolchain.toml'; aterm-managed store build 9192 under {})",
        lab.p("atpkg")
    );
    let routes: [(&[&str], &str); 4] = [
        (&["shadow", "atpkg-cli"], "nohome"),
        (&["shadow", "atpkg/bin"], "nohome"),
        (&["atpkg/store/trust/current/bin"], "nohome"),
        (&["shadow", "atpkg-cli"], "cargo-home"),
    ];
    let mut lib = lab.lib();
    let gates: Vec<Gate> = routes
        .iter()
        .map(|(dirs, home)| gate(&mut lib, &lab, &lab.path(dirs), home, &root, "", true))
        .collect();
    let path = lab.path(&["shadow", "atpkg-cli"]);
    lib.step(format!(
        "export PATH={} CARGO_HOME={}",
        q(&path),
        q(&lab.p("nohome"))
    ));
    lib.step(format!(
        "find_cargo_and_rustup {r} && cargo_honours_pin {r}",
        r = q(&root)
    ));
    let implicit = lib.call(
        format!(
            "(cd {r} && \"$CARGO_BIN\" build --release -p aterm --target-dir {r}/target)",
            r = q(&root)
        ),
        &[],
    );
    lib.step(": >\"$SHIM_LOG\"; export RUSTC=/usr/bin/false CARGO_BUILD_RUSTC=/usr/bin/false");
    let steered = lib.call(
        format!("(cd {r} && \"$CARGO_BIN\" --unverified build --release -p aterm --target-dir {r}/target)", r = q(&root)),
        &[],
    );
    let steered_log = lib.call("cat \"$SHIM_LOG\"; : >\"$SHIM_LOG\"", &[]);
    let build = lib.call(format!("cargo_build_pinned {}", q(&root)), &["PATH"]);
    let build_log = lib.call("cat \"$SHIM_LOG\"; : >\"$SHIM_LOG\"", &[]);
    let recs = lib.run(&scratch);
    for ((dirs, home), g) in routes.iter().zip(&gates) {
        let label = format!("{dirs:?} CARGO_HOME {home}");
        let found = Found::from(&recs[g.find]);
        assert!(found.ok, "{label}: {}", found.reason);
        assert_eq!(
            (
                found.lane.as_str(),
                found.cargo.as_str(),
                found.rustup.as_str()
            ),
            ("store", format!("{store_bin}/targo").as_str(), ""),
            "{label}"
        );
        assert_eq!(
            (found.prefix.as_str(), found.build.as_str()),
            (lab.p("atpkg").as_str(), "9192"),
            "{label}"
        );
        let proved = Proved::from(&recs[g.proof.expect("proved")]);
        assert!(proved.ok, "{label}: {}", proved.reason);
        assert_eq!(
            (
                proved.version.as_str(),
                proved.toolchain.as_str(),
                proved.rustc.as_str()
            ),
            (
                STORE_TARGO_VERSION,
                toolchain.as_str(),
                format!("{store_bin}/trustc").as_str()
            ),
            "{label}"
        );
    }
    assert_eq!(recs[implicit].rc, 1, "targo refuses the implicit lane");
    assert_eq!(recs[steered].rc, 97);
    assert!(
        recs[steered_log]
            .out
            .contains("STEERED: RUSTC=/usr/bin/false"),
        "{}",
        recs[steered_log].out
    );
    assert_eq!(recs[build].rc, 97, "the build's own exit status");
    assert_eq!(
        recs[build_log].out,
        format!(
            "targo[9192] --unverified build --release -p aterm --target-dir {root}/target (PATH first: {store_bin})"
        )
    );
    assert_eq!(
        recs[build].var("PATH"),
        path,
        "the caller's PATH is untouched"
    );
}

/// atpkg's macOS `targo` shim runs aterm's build-dir block before its exec (the block is
/// atpkg's own, `build_cache::shim_block`, so the shim writer and this reader answer to
/// one contract): about a hundred and twenty lines of builtins, the exec last.
#[test]
fn a_shim_is_followed_to_its_last_lines_exec_only_in_the_decided_shape() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let root = s(&lab.checkout).to_string();
    let store_targo = format!("{}/targo", lab.store_bin());
    let head = "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.";
    let exec = format!("exec '{store_targo}' \"$@\"");
    let block = atpkg::build_cache::shim_block("targo").expect("targo's block");
    assert!(
        block.starts_with("# aterm: build intermediates out of Spotlight")
            && block.contains("set -- --config"),
        "{block}"
    );
    let stray = lab.p("stray-targo/targo");
    let shims = [
        ("prelude", format!("{head}\n{block}{exec}")),
        (
            "two-exec",
            format!("{head}\n[ -f '{stray}' ] && exec '{stray}' \"$@\"\n{exec}"),
        ),
        ("exec-not-last", format!("{head}\n{exec}\n: after")),
        (
            "relative",
            format!("{head}\nexec 'atpkg/store/trust/9192/bin/targo' \"$@\""),
        ),
        ("unquoted", format!("{head}\nexec {store_targo} \"$@\"")),
        ("bash", format!("#!/bin/bash\n{exec}")),
        ("here-document", format!("{head}\ncat <<EOF\nEOF\n{exec}")),
        ("continued", format!("{head}\ntrue &&\n{exec}")),
    ];
    let shim = |name: &str| lab.p(&format!("shims/{name}/targo"));
    for (name, body) in &shims {
        scratch.exe(&format!("lab/shims/{name}/targo"), &format!("{body}\n"));
    }
    let mut lib = lab.lib();
    lib.step(format!(
        "mkdir -p {dir} && {{ printf '%s\\n#' {head}; head -c \"$GATE_SHIM_MAX_BYTES\" /dev/zero | tr '\\0' x; printf '\\n%s\\n' {exec}; }} >{dir}/targo && chmod +x {dir}/targo",
        dir = q(&lab.p("shims/oversize")),
        head = q(head),
        exec = q(&exec),
    ));
    let names: Vec<&str> = shims
        .iter()
        .map(|(name, _)| *name)
        .chain(["oversize"])
        .collect();
    let read: Vec<usize> = names
        .iter()
        .map(|name| lib.call(format!("gate_shim_target {}", q(&shim(name))), &[]))
        .collect();
    let prelude = gate(
        &mut lib,
        &lab,
        &lab.path(&["shims/prelude"]),
        "nohome",
        &root,
        "",
        false,
    );
    let two_exec = gate(
        &mut lib,
        &lab,
        &lab.path(&["shims/two-exec"]),
        "nohome",
        &root,
        "",
        false,
    );
    let recs = lib.run(&scratch);
    for (name, at) in names.iter().zip(&read) {
        let want = if *name == "prelude" {
            store_targo.clone()
        } else {
            shim(name)
        };
        assert_eq!(recs[*at].out, want, "a {name} shim");
    }
    let found = Found::from(&recs[prelude.find]);
    assert!(
        found.ok,
        "the build-dir shim on PATH resolves: {}",
        found.reason
    );
    assert_eq!(
        (found.lane.as_str(), found.cargo.as_str()),
        ("store", store_targo.as_str()),
        "its final exec names the store's targo"
    );
    let found = Found::from(&recs[two_exec.find]);
    assert!(
        !found.ok,
        "a second exec never takes the managed lane: {}",
        found.cargo
    );
    assert!(
        found.reason.contains(&format!(
            "the targo in reach was declined: the targo in reach, {}, is not in an \
             aterm-managed store",
            shim("two-exec")
        )),
        "{}",
        found.reason
    );
}

/// A targo that is not the store's never takes the lane: alone on PATH it is
/// declined by name; ahead of the store's shim it does not hide it; and when
/// atpkg answers SHADOWED, the alias names the managed copy (or, with no alias
/// laid, the PATH scan finds the shim).
#[test]
fn the_store_is_found_past_a_stray_targo_and_a_damaged_one_is_refused() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let root = s(&lab.checkout).to_string();
    let targo = format!("{}/targo", lab.store_bin());
    let mut lib = lab.lib();
    let alone = gate(
        &mut lib,
        &lab,
        &lab.path(&["stray-targo"]),
        "nohome",
        &root,
        "",
        false,
    );
    let ahead = gate(
        &mut lib,
        &lab,
        &lab.path(&["stray-targo", "atpkg/bin"]),
        "nohome",
        &root,
        "",
        false,
    );
    let shadowed = gate(
        &mut lib,
        &lab,
        &lab.path(&["stray-targo", "atpkg-cli"]),
        "nohome",
        &root,
        "",
        false,
    );
    let no_alias = gate(
        &mut lib,
        &lab,
        &lab.path(&["stray-targo", "atpkg-cli", "atpkg/bin"]),
        "nohome",
        &root,
        "export FAKE_NO_ALIAS=1",
        false,
    );
    let broken = gate(
        &mut lib,
        &lab,
        &lab.path(&["atpkg-broken/bin"]),
        "nohome",
        &root,
        "",
        true,
    );
    let recs = lib.run(&scratch);
    let found = Found::from(&recs[alone.find]);
    assert!(!found.ok && found.lane == "rustup", "{found:?}");
    let (remedy, declined) = found
        .reason
        .split_once("; the targo in reach was declined: ")
        .unwrap_or_else(|| panic!("the declined targo is named: {}", found.reason));
    assert_eq!(
        remedies(remedy),
        ["https://rustup.rs", "rustup toolchain link trust <prefix>"],
        "{remedy}"
    );
    assert!(declined.contains(&lab.p("stray-targo/targo")), "{declined}");
    for (label, g) in [
        ("stray ahead of the shim", &ahead),
        ("SHADOWED", &shadowed),
        ("SHADOWED, no alias", &no_alias),
    ] {
        let found = Found::from(&recs[g.find]);
        assert!(found.ok, "{label}: {}", found.reason);
        assert_eq!(
            (
                found.lane.as_str(),
                found.cargo.as_str(),
                found.decline.as_str()
            ),
            ("store", targo.as_str(), ""),
            "{label}"
        );
    }
    let found = Found::from(&recs[broken.find]);
    assert_eq!((found.ok, found.lane.as_str()), (true, "store"));
    let proved = Proved::from(&recs[broken.proof.expect("proved")]);
    assert!(!proved.ok && proved.rustc.is_empty(), "{proved:?}");
    assert!(
        proved
            .reason
            .contains(&lab.p("atpkg-broken/store/trust/9193/bin/trustc")),
        "{}",
        proved.reason
    );
    assert_eq!(
        remedies(&proved.reason),
        ["aterm pkg install trust", "aterm pkg doctor"]
    );
}

/// A stock pin is not the store's toolchain: with the store in reach the lane
/// is declined by its pin and rustup's proof runs; with no rustup the refusal
/// offers rustup and the Homebrew way out, never the store.
#[test]
fn a_stock_pin_never_takes_the_store_lane() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let stock = lab.p("stock-pin");
    let mut lib = lab.lib();
    let proxies = gate(
        &mut lib,
        &lab,
        &lab.path(&["shadow", "atpkg-cli"]),
        "cargo-home",
        &stock,
        "",
        true,
    );
    let no_rustup = gate(
        &mut lib,
        &lab,
        &lab.path(&["shadow", "atpkg-cli"]),
        "nohome",
        &stock,
        "",
        false,
    );
    let recs = lib.run(&scratch);
    let found = Found::from(&recs[proxies.find]);
    assert_eq!(
        (found.lane.as_str(), found.cargo),
        ("rustup", lab.p("cargo-home/bin/cargo"))
    );
    assert!(
        found.decline.contains("pins 'stable', not 'trust'"),
        "{}",
        found.decline
    );
    let proved = Proved::from(&recs[proxies.proof.expect("proved")]);
    assert!(proved.ok, "{}", proved.reason);
    assert_eq!(
        proved.toolchain,
        format!("{PIN} (overridden by '{stock}/rust-toolchain.toml')")
    );
    assert_eq!(proved.rustc, lab.p("cargo-home/bin/rustc"));
    let found = Found::from(&recs[no_rustup.find]);
    assert!(!found.ok);
    assert_eq!(
        remedies(&found.reason),
        ["https://rustup.rs", "brew uninstall rust"],
        "{}",
        found.reason
    );
}

/// With no toolchain in reach at all, the refusal names ONE remedy: the
/// product's where an `aterm` can run it, else rustup and the Trust tarballs
/// (rustup alone cannot fetch the `trust` pin) — and on Linux, where neither is
/// published, rustup and a Trust built from source, `aterm` or not; and a
/// rustup with no proxy beside it says where the proxy should have been.
#[test]
fn with_no_toolchain_in_reach_the_one_remedy_depends_on_an_aterm() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let root = s(&lab.checkout).to_string();
    let mut lib = lab.lib();
    let rows = [
        (
            gate(
                &mut lib,
                &lab,
                &lab.path(&["none"]),
                "nohome",
                &root,
                "",
                false,
            ),
            vec!["https://rustup.rs", "rustup toolchain link trust <prefix>"],
        ),
        (
            gate(
                &mut lib,
                &lab,
                &lab.path(&["none", "bare-aterm"]),
                "nohome",
                &root,
                "",
                false,
            ),
            vec!["aterm pkg install trust"],
        ),
        (
            gate(
                &mut lib,
                &lab,
                &lab.path(&["none"]),
                "nohome",
                "",
                "",
                false,
            ),
            vec!["https://rustup.rs", "rustup toolchain link trust <prefix>"],
        ),
        (
            gate(
                &mut lib,
                &lab,
                &lab.path(&["none", "bare-aterm"]),
                "nohome",
                "",
                "",
                false,
            ),
            vec!["aterm pkg install trust"],
        ),
    ];
    let linux = [
        gate(
            &mut lib,
            &lab,
            &lab.path(&["linux", "none"]),
            "nohome",
            &root,
            "",
            false,
        ),
        gate(
            &mut lib,
            &lab,
            &lab.path(&["linux", "none", "bare-aterm"]),
            "nohome",
            &root,
            "",
            false,
        ),
    ];
    let bare = gate(
        &mut lib,
        &lab,
        &lab.path(&["bare-rustup"]),
        "nohome",
        "",
        "",
        false,
    );
    let recs = lib.run(&scratch);
    for (g, want) in &rows {
        let found = Found::from(&recs[g.find]);
        assert!(!found.ok, "{found:?}");
        assert_eq!(&remedies(&found.reason), want, "{}", found.reason);
    }
    for g in &linux {
        let found = Found::from(&recs[g.find]);
        assert!(!found.ok, "{found:?}");
        assert_eq!(
            remedies(&found.reason),
            ["https://rustup.rs", "rustup toolchain link trust <prefix>"],
            "{}",
            found.reason
        );
        assert!(
            says(
                &found.reason,
                &["publishes no build for Linux"],
                &["tarballs"]
            ),
            "{}",
            found.reason
        );
    }
    let found = Found::from(&recs[bare.find]);
    assert!(!found.ok);
    assert!(
        found.reason.contains(&format!(
            "{}/rustup (at {}/cargo)",
            lab.p("bare-rustup"),
            lab.p("bare-rustup")
        )),
        "{}",
        found.reason
    );
    assert!(
        found
            .reason
            .contains("reinstall rustup (https://rustup.rs)"),
        "{}",
        found.reason
    );
}

/// The source build as `install.sh --dry-run` plans it.
#[derive(Debug, PartialEq)]
enum Plan {
    /// `toolchain <name>: <version> (<cargo>)`.
    Build {
        toolchain: String,
        version: String,
        cargo: String,
    },
    /// The skip and its reason.
    Skip(String),
}

/// A checkout copy for whole-script runs: install.sh, the workspace manifest
/// (the channel it installs from) and `pin`.
fn checkout(scratch: &Scratch, name: &str, pin: &str) -> PathBuf {
    let dir = scratch.mkdir(&format!("{name}/tools"));
    std::fs::copy(install_sh(), dir.join("install.sh")).expect("copy install.sh");
    let dir = scratch.join(name);
    std::fs::copy(root().join("Cargo.toml"), dir.join("Cargo.toml")).expect("copy Cargo.toml");
    write(
        &dir.join("rust-toolchain.toml"),
        format!("[toolchain]\nchannel = \"{pin}\"\n"),
    );
    dir
}

/// One whole-script run: the checkout it runs from, the PATH (lab dirs),
/// `$CARGO_HOME`, extra environment, and the plan it must print.
struct DryRun<'a> {
    label: &'a str,
    checkout: &'a Path,
    dirs: &'a [&'a str],
    home: &'a str,
    extra: Vec<(&'a str, &'a str)>,
    want: Want,
}

enum Want {
    Build(Plan),
    /// A skip whose reason names each of `names`, in order, and none of `lacks`.
    Skip {
        names: Vec<String>,
        lacks: Vec<String>,
    },
}

impl Want {
    fn skip(names: &[&str], lacks: &[&str]) -> Self {
        Self::Skip {
            names: names.iter().map(|n| (*n).to_string()).collect(),
            lacks: lacks.iter().map(|n| (*n).to_string()).collect(),
        }
    }

    fn holds(&self, plan: &Plan) -> bool {
        match (self, plan) {
            (Self::Build(want), plan) => want == plan,
            (Self::Skip { names, lacks }, Plan::Skip(reason)) => says(reason, names, lacks),
            (Self::Skip { .. }, Plan::Build { .. }) => false,
        }
    }
}

/// `install.sh --dry-run --no-toolchain --no-app` from the row's checkout: its
/// plan for the source build, after checking it exited 0, wrote nothing, and
/// never announced a build beside a skip.
fn dry_run(scratch: &Scratch, lab: &Lab, n: usize, row: &DryRun) -> Plan {
    let fresh: Home = scratch.home(&format!("dry/{n}"));
    let path = lab.path(row.dirs);
    let cargo_home = lab.p(row.home);
    let stand_ins = lab.env();
    let mut env: Vec<(&str, &str)> = vec![("PATH", &path), ("CARGO_HOME", &cargo_home)];
    env.extend(stand_ins.iter().map(|(k, v)| (*k, v.as_str())));
    env.extend_from_slice(&row.extra);
    let script = row.checkout.join("tools/install.sh");
    let run = installer(
        &script,
        &fresh,
        &env,
        &["--dry-run", "--no-toolchain", "--no-app"],
    );
    let out = run.all();
    let label = row.label;
    assert_eq!(run.code(), 0, "{label}: {out}");
    assert_eq!(tree(&fresh.home), Vec::<String>::new(), "{label}: HOME");
    assert_eq!(tree(&fresh.tmp), Vec::<String>::new(), "{label}: TMPDIR");
    let skip = out
        .lines()
        .find_map(|l| l.strip_prefix("  cargo:    the source build would be SKIPPED: "));
    let build = out.lines().find_map(|l| l.strip_prefix("  toolchain "));
    match (skip, build) {
        (Some(reason), None) => Plan::Skip(reason.into()),
        (None, Some(line)) => {
            let (toolchain, rest) = line.split_once(": ").expect("toolchain: version (cargo)");
            let (version, cargo) = rest.rsplit_once(" (").expect("version (cargo)");
            Plan::Build {
                toolchain: toolchain.into(),
                version: version.into(),
                cargo: cargo.trim_end_matches(')').into(),
            }
        }
        _ => panic!("{label}: the plan names the source build once, as a skip or a build: {out}"),
    }
}

/// The plan tells the truth about the cargo: every refusal is the skip it is,
/// with its remedy, and a refused cargo is never announced as the build's;
/// every proved toolchain is announced with its version and cargo.
#[test]
fn the_dry_run_plan_reports_the_source_build_as_the_run_would() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let trust = checkout(&scratch, "trust-checkout", "trust");
    let stock = checkout(&scratch, "stock-checkout", "1.97.1");
    let ro = scratch.mkdir("ro");
    chmod(&ro, 0o555);
    let ro_store = format!("{}/store", s(&ro));
    let bare_aterm = lab.p("bare-aterm");
    let prepend = format!("export PATH=\"{}:$PATH\"", lab.p("bare-rustup"));
    let twin = format!("{} ({PIN_VERSION})", lab.p("twin/cargo"));
    let rustc_less = lab.p("rustc-less/cargo");
    let unwritable = format!("cannot create/write {ro_store}");
    let rustup_remedy = ["https://rustup.rs", "rustup toolchain link trust <prefix>"];
    let product = "aterm pkg install trust";
    let pinned = |cargo: &str| {
        Want::Build(Plan::Build {
            toolchain: PIN.into(),
            version: PIN_VERSION.into(),
            cargo: lab.p(cargo),
        })
    };
    let stock_pin = ("FAKE_TOOLCHAIN_NAME", "1.97.1-x86_64-apple-darwin");
    let row = |label, checkout, dirs, home, extra, want| DryRun {
        label,
        checkout,
        dirs,
        home,
        extra,
        want,
    };
    let mut rows = vec![
        row(
            "shadow beside a proxy-less rustup",
            &trust,
            &["shadow", "bare-rustup"],
            "nohome",
            vec![],
            Want::skip(&[&prepend], &[]),
        ),
        row(
            "shadow, no rustup",
            &trust,
            &["shadow"],
            "nohome",
            vec![],
            Want::skip(&rustup_remedy, &[product]),
        ),
        row(
            "the twin",
            &trust,
            &["twin", "bare-rustup"],
            "nohome",
            vec![],
            Want::skip(&[&twin], &[]),
        ),
        row(
            "the trust pin absent",
            &trust,
            &["shadow"],
            "cargo-home",
            vec![("FAKE_TOOLCHAIN_MISSING", "1")],
            Want::skip(&[product, "rustup toolchain link trust <prefix>"], &[]),
        ),
        row(
            "the trust pin absent, on Linux",
            &trust,
            &["linux", "shadow"],
            "cargo-home",
            vec![("FAKE_TOOLCHAIN_MISSING", "1")],
            Want::skip(
                &[
                    "publishes no build for Linux",
                    "rustup toolchain link trust <prefix>",
                ],
                &[product, "tarballs"],
            ),
        ),
        row(
            "a stock pin absent",
            &stock,
            &["shadow"],
            "cargo-home",
            vec![stock_pin, ("FAKE_TOOLCHAIN_MISSING", "1")],
            Want::skip(
                &["rustup toolchain install 1.97.1"],
                &["alabsystems/trust", "rustup toolchain link"],
            ),
        ),
        row(
            "a rustc-less proxy pair",
            &trust,
            &["shadow", "rustc-less"],
            "nohome",
            vec![],
            Want::skip(&[&rustc_less], &[]),
        ),
        row(
            "a rustup override",
            &trust,
            &["shadow"],
            "cargo-home",
            vec![("FAKE_DIR_OVERRIDE", FOREIGN)],
            Want::skip(&["rustup override unset --path"], &[]),
        ),
        row(
            "an unwritable store",
            &trust,
            &["shadow"],
            "cargo-home",
            vec![("ATERM_STORE_DIR", &ro_store)],
            Want::skip(&[&unwritable], &[]),
        ),
        row(
            "the proxies at CARGO_HOME",
            &trust,
            &["shadow"],
            "cargo-home",
            vec![],
            pinned("cargo-home/bin/cargo"),
        ),
        row(
            "a keg rustup behind the shadow",
            &trust,
            &["shadow", "rustup-keg"],
            "nohome",
            vec![],
            pinned("rustup-keg/cargo"),
        ),
        row(
            "an ambient RUSTUP_TOOLCHAIN",
            &trust,
            &["shadow"],
            "cargo-home",
            vec![("RUSTUP_TOOLCHAIN", FOREIGN)],
            pinned("cargo-home/bin/cargo"),
        ),
        row(
            "the managed store",
            &trust,
            &["shadow", "atpkg-cli"],
            "nohome",
            vec![],
            Want::Build(Plan::Build {
                toolchain: "trust".into(),
                version: STORE_TARGO_VERSION.into(),
                cargo: format!("{}/targo", lab.store_bin()),
            }),
        ),
        row(
            "a stock pin with the store in reach",
            &stock,
            &["shadow", "atpkg-cli"],
            "cargo-home",
            vec![stock_pin],
            Want::Build(Plan::Build {
                toolchain: "1.97.1-x86_64-apple-darwin".into(),
                version: PIN_VERSION.into(),
                cargo: lab.p("cargo-home/bin/cargo"),
            }),
        ),
    ];
    let flipped = case_changed(&trust);
    if let Some(flipped) = &flipped {
        rows.push(row(
            "the checkout through a case-changed path",
            flipped,
            &["shadow"],
            "cargo-home",
            vec![],
            pinned("cargo-home/bin/cargo"),
        ));
    }
    rows.push(row(
        "neither lane, no aterm",
        &trust,
        &["none"],
        "nohome",
        vec![],
        Want::skip(&rustup_remedy, &[product]),
    ));
    rows.push(row(
        "neither lane, aterm in BIN_DIR",
        &trust,
        &["none"],
        "nohome",
        vec![("ATERM_BIN_DIR", &bare_aterm)],
        Want::skip(&[product], &["https://rustup.rs"]),
    ));
    let plans: Vec<Plan> = std::thread::scope(|scope| {
        let handles: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(n, row)| {
                let (lab, scratch) = (&lab, &scratch);
                scope.spawn(move || dry_run(scratch, lab, n, row))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a dry run"))
            .collect()
    });
    chmod(&ro, 0o755);
    let wrong: Vec<String> = rows
        .iter()
        .zip(&plans)
        .filter(|(row, plan)| !row.want.holds(plan))
        .map(|(row, plan)| format!("{}: {plan:?}", row.label))
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert_eq!(
        lab.take_builds(),
        "",
        "a dry run never builds or reaches the network"
    );
}

/// A source build that fails ends the run with the installer's own line — what
/// failed and that the command was not installed — and status 1, not with
/// cargo's status and its error as the run's last words.
#[test]
fn a_failed_source_build_is_said_and_installs_nothing() {
    let scratch = Scratch::new();
    let lab = Lab::new(&scratch);
    let trust = checkout(&scratch, "build-checkout", "trust");
    let fresh: Home = scratch.home("build");
    let path = lab.path(&["linux", "shadow"]);
    let cargo_home = lab.p("cargo-home");
    let stand_ins = lab.env();
    let mut env: Vec<(&str, &str)> = vec![("PATH", &path), ("CARGO_HOME", &cargo_home)];
    env.extend(stand_ins.iter().map(|(k, v)| (*k, v.as_str())));
    let run = installer(
        &trust.join("tools/install.sh"),
        &fresh,
        &env,
        &["--no-toolchain", "--no-app", "--no-path"],
    );
    let out = run.all();
    assert_eq!(run.code(), 1, "{out}");
    assert!(
        out.contains(
            "install.sh: the source build failed (cargo exit 97, its error above) — the `aterm` \
             command was not installed"
        ),
        "{out}"
    );
    assert!(
        lab.take_builds().contains("build --release -p aterm"),
        "the build ran"
    );
    assert!(
        !fresh.home.join(".local/bin/aterm").exists(),
        "nothing was linked"
    );
}
