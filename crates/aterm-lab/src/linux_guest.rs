// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the Linux lane's VM holds and how a command runs in it, as data: the provisioning plan (apt sources, packages, the Rust toolchain), the environment per architecture, and the display sessions.

use std::fmt::Write as _;

/// A configuration namespace reserved by one `Up` with guest `mktemp -d`.
#[derive(Debug)]
pub struct Config {
    root: String,
}

impl Config {
    /// The command that reserves the run's private directory in the guest.
    #[must_use]
    pub fn reserve() -> Vec<String> {
        strings(&["mktemp", "-d", "--", "/tmp/aterm-lane-config.XXXXXXXXXX"])
    }

    /// Read the successful reservation's single path, without shell expansion.
    #[must_use]
    pub fn from_created(text: &str) -> Option<Config> {
        let root = text.strip_suffix('\n').unwrap_or(text);
        let suffix = root.strip_prefix("/tmp/aterm-lane-config.")?;
        (suffix.len() == 10 && suffix.bytes().all(|b| b.is_ascii_alphanumeric()))
            .then(|| Config { root: root.into() })
    }

    /// Whether a file is strictly inside this run's namespace, without a parent component or an empty component that obscures its spelling.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        path.strip_prefix(&self.root)
            .and_then(|s| s.strip_prefix('/'))
            .is_some_and(|s| {
                s.split('/')
                    .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains('\0'))
            })
    }

    #[must_use]
    pub fn weston_ini(&self) -> String {
        format!("{}/weston/weston.ini", self.root)
    }

    #[must_use]
    fn mutter(&self) -> String {
        format!("{}/mutter", self.root)
    }

    #[must_use]
    fn mutter_plain(&self) -> String {
        format!("{}/mutter-plain", self.root)
    }

    /// IBus's daemon, XIM server and engine command share this run's settings and bus-address directory.
    #[must_use]
    pub fn ibus(&self) -> String {
        format!("{}/ibus", self.root)
    }
}

/// STOCK EXCEPTION: no Linux build of the Trust toolchain is published (the atpkg `trust` releases carry the aarch64-apple-darwin asset alone), and Trust ships only its host's std, so the guest compiles with rustup's stock toolchain at this version, plus the x86_64 std.
pub const RUST: &str = "1.99.1";

/// A CPU architecture the lane tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    /// The guest's own: built and run natively.
    Aarch64,
    /// Cross-built with the x86_64 GNU toolchain and run under Rosetta: the cell the owner ships first.
    X86_64,
}

impl Arch {
    /// Both, in the order a run takes them.
    pub const ALL: [Arch; 2] = [Arch::Aarch64, Arch::X86_64];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64",
            Arch::X86_64 => "x86_64",
        }
    }

    /// The Rust target triple.
    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64-unknown-linux-gnu",
            Arch::X86_64 => "x86_64-unknown-linux-gnu",
        }
    }

    /// How its binaries run in the guest.
    #[must_use]
    pub const fn runs(self) -> &'static str {
        match self {
            Arch::Aarch64 => "native",
            Arch::X86_64 => "Rosetta",
        }
    }

    /// The architecture a command-line word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Arch> {
        match word {
            "aarch64" | "arm64" => Some(Arch::Aarch64),
            "x86_64" | "amd64" | "x64" => Some(Arch::X86_64),
            _ => None,
        }
    }

    /// The `cargo` arguments that select this architecture's target: none for the guest's own, which `cargo` builds by default.
    #[must_use]
    pub fn target_args(self) -> Vec<String> {
        match self {
            Arch::Aarch64 => Vec::new(),
            Arch::X86_64 => vec!["--target".into(), self.triple().into()],
        }
    }

    /// Where `cargo` puts this architecture's binaries under `target_dir`.
    #[must_use]
    pub fn bin_dir(self, target_dir: &str) -> String {
        match self {
            Arch::Aarch64 => format!("{target_dir}/debug"),
            Arch::X86_64 => format!("{target_dir}/{}/debug", self.triple()),
        }
    }
}

/// The guest's arm64 sources, written over the image's own (which name no architecture, and so would also be asked for amd64 indexes that ports.ubuntu.com does not carry).
pub const SOURCES_ARM64: &str = "\
Types: deb
Architectures: arm64
URIs: http://ports.ubuntu.com/ubuntu-ports
Suites: noble noble-updates noble-backports
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
Architectures: arm64
URIs: http://ports.ubuntu.com/ubuntu-ports
Suites: noble-security
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
";

/// The amd64 sources, for the x86_64 libraries.
pub const SOURCES_AMD64: &str = "\
Types: deb
Architectures: amd64
URIs: http://archive.ubuntu.com/ubuntu
Suites: noble noble-updates noble-backports
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
Architectures: amd64
URIs: http://security.ubuntu.com/ubuntu
Suites: noble-security
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
";

/// The development libraries the X11, Wayland and xkb code links or loads (the winit fork, softbuffer, x11-dl's `build.rs` probes, wgpu's GL and Vulkan loaders), installed for both architectures.
pub const DEV_LIBRARIES: &[&str] = &[
    "libx11-dev",
    "libx11-xcb-dev",
    "libxcursor-dev",
    "libxrandr-dev",
    "libxi-dev",
    "libxext-dev",
    "libxrender-dev",
    "libxfixes-dev",
    "libxinerama-dev",
    "libxxf86vm-dev",
    "libxcb1-dev",
    "libxkbcommon-dev",
    "libxkbcommon-x11-dev",
    "libwayland-dev",
    "libegl-dev",
    "libgl-dev",
    "libvulkan-dev",
];

/// Mesa's software drivers, installed for both architectures: llvmpipe (GL) and lavapipe (Vulkan), the lane's GPU.
pub const MESA: &[&str] = &["libgl1-mesa-dri", "libegl-mesa0", "mesa-vulkan-drivers"];

/// The guest's own tools: the build, the display servers, the second X client of the clipboard round trip, the baseline's input and probe tools (xdotool's XTEST keys and pointer, xset and xrdb, xdpyinfo, wayland-info, wl-clipboard's second Wayland client, ibus as the XIM server, GNOME Shell, whose IBus bridge is the one input method that reaches Mutter's text-input-v3, and pipewire, without which Mutter has no screencast, the space RemoteDesktop places a finger in), the session bus and the accessibility bus, Orca with speech-dispatcher (whose dummy output module answers speech with no audio device) and xdg-desktop-portal with its GTK backend (the Settings portal aterm's motion read asks), the GPU probes, fonts (a stock Ubuntu desktop's: Noto CJK and Noto Color Emoji are the CJK and colour faces aterm-render's script and emoji tests need, and without them those answer COULD NOT RUN), the x86_64 cross toolchain with its libc, zsh and fish, the shells aterm integrates beside bash, whose integration tests run each one and cannot run without it (apt's fish, 3.7 on noble, is the fish floor lane, docs/DESIGN-init-system-2026-10-20.md §6.2: the guest never takes the pinned 4.9.3 `verify provision` builds on macOS; pwsh, the fourth, is [`PWSH`], Microsoft's release archive held to its sha256), the two build tools [`BASH_3_2_INSTALL`] needs (autotools-dev's `config.guess` and `config.sub`, which know aarch64, and bison), unzip, which `verify provision` unpacks the browser lane's pinned browsers with, sway (headless wlroots: the one lane compositor that grants server-side decorations and offers cursor-shape-v1 and a virtual pointer, for `linux edges`), grim (its screenshot, for the shadow margin's pixels), GStreamer's pipewire source and PNG encoder (a frame of Mutter's screencast, the pixels a Mutter CPU row is held to), ibus-libpinyin (a CJK engine with a preedit, for XIM), xtrace (a proxy display that logs an X11 client's requests, for the baseline's `x11.wire.*` lines, [`crate::linux_xwire`]), and python3 and openssl, the install_sh tests' floor for install.sh's embedded verifier (Python 3.11+, OpenSSL 3: docs/DESIGN-init-system-2026-10-20.md §6.1 row 14).
pub const TOOLS: &[&str] = &[
    "build-essential",
    "pkg-config",
    "ca-certificates",
    "curl",
    "unzip",
    "rustup",
    "xvfb",
    "xauth",
    "xwayland",
    "xclip",
    "xdotool",
    "x11-xkb-utils",
    "x11-utils",
    "x11-xserver-utils",
    "wayland-utils",
    "wl-clipboard",
    "ibus",
    "weston",
    "mutter",
    "gnome-shell",
    "pipewire",
    "wireplumber",
    "dbus",
    "dbus-x11",
    "dbus-user-session",
    "at-spi2-core",
    "orca",
    "speech-dispatcher",
    "xdg-desktop-portal",
    "xdg-desktop-portal-gtk",
    "mesa-utils",
    "vulkan-tools",
    "fonts-dejavu-core",
    "fonts-noto-core",
    "fonts-noto-cjk",
    "fonts-noto-color-emoji",
    "gcc-x86-64-linux-gnu",
    "libc6-dev-amd64-cross",
    "zsh",
    "fish",
    "autotools-dev",
    "bison",
    "sway",
    "grim",
    "gstreamer1.0-tools",
    "gstreamer1.0-pipewire",
    "gstreamer1.0-plugins-good",
    "ibus-libpinyin",
    "xtrace",
    "python3",
    "openssl",
];

/// x86_64 packages beyond the shared lists: the loader and libc the translated binaries start on, and the C++ runtime Mesa links.
pub const AMD64_RUNTIME: &[&str] = &["libc6", "libgcc-s1", "libstdc++6"];

/// The libraries the browser lane's x86_64 browser loads (`linux browser`): every `NEEDED` entry of Chrome for Testing's linux64 chrome-headless-shell and the libraries it ships (`readelf -d`, pin 155.0.8059.84) that the archive does not carry, as amd64 packages.
pub const BROWSER_AMD64: &[&str] = &[
    "libasound2t64",
    "libatk-bridge2.0-0t64",
    "libatk1.0-0t64",
    "libatspi2.0-0t64",
    "libdbus-1-3",
    "libexpat1",
    "libgbm1",
    "libglib2.0-0t64",
    "libnspr4",
    "libnss3",
    "libudev1",
    "libx11-6",
    "libxcb1",
    "libxcomposite1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxkbcommon0",
    "libxrandr2",
];

/// The libraries the browser lane's aarch64 browser loads: every `NEEDED` entry of Playwright's Firefox for Ubuntu 24.04 on arm64 (build 1543) that the archive does not carry, as the guest's own packages.
pub const BROWSER_ARM64: &[&str] = &[
    "libasound2t64",
    "libatk1.0-0t64",
    "libcairo-gobject2",
    "libcairo2",
    "libdbus-1-3",
    "libfontconfig1",
    "libfreetype6",
    "libgdk-pixbuf-2.0-0",
    "libglib2.0-0t64",
    "libgtk-3-0t64",
    "libpango-1.0-0",
    "libpangocairo-1.0-0",
    "libx11-6",
    "libx11-xcb1",
    "libxcb-shm0",
    "libxcb1",
    "libxcomposite1",
    "libxcursor1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxi6",
    "libxrandr2",
    "libxrender1",
];

/// Every package the guest installs, `:amd64` names last.
#[must_use]
pub fn packages() -> Vec<String> {
    let native = TOOLS
        .iter()
        .chain(DEV_LIBRARIES)
        .chain(MESA)
        .chain(BROWSER_ARM64);
    let foreign = DEV_LIBRARIES
        .iter()
        .chain(MESA)
        .chain(AMD64_RUNTIME)
        .chain(BROWSER_AMD64);
    native
        .map(|p| (*p).to_string())
        .chain(foreign.map(|p| format!("{p}:amd64")))
        .collect()
}

/// Where the guest records the plan it was provisioned with.
pub const STAMP: &str = "/var/lib/aterm-lane/provisioned";

/// PowerShell 7, the fourth shell aterm integrates: aterm-shell-integration's pwsh tests answer COULD NOT RUN without one, and Ubuntu packages none for arm64 (Microsoft's apt repository carries amd64 alone), so the guest gets the release's own linux-arm64 archive, held to the digest GitHub publishes for it ([`PWSH_INSTALL`]). The pin is a release at least two weeks old.
pub const PWSH: (&str, &str) = (
    "7.6.6",
    "924829e54c983648f6f1419a2dc7f9433c861b2fb5bd57736ff096c24f133729",
);

/// Install [`PWSH`] (`$1` its version, `$2` the archive's sha256) at `/opt/aterm-lane/pwsh-<version>` and link it as `/usr/local/bin/pwsh`, run as root.
pub const PWSH_INSTALL: &str = r#"set -eu
dir=/opt/aterm-lane/pwsh-$1
if [ ! -x "$dir/pwsh" ]; then
  mkdir -p /opt/aterm-lane
  tmp=$(mktemp -d /opt/aterm-lane/.pwsh.XXXXXX)
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL -o "$tmp/pwsh.tar.gz" \
    "https://github.com/PowerShell/PowerShell/releases/download/v$1/powershell-$1-linux-arm64.tar.gz"
  echo "$2  $tmp/pwsh.tar.gz" | sha256sum -c --quiet
  mkdir "$tmp/pwsh"
  tar -xzf "$tmp/pwsh.tar.gz" -C "$tmp/pwsh"
  chmod 0755 "$tmp/pwsh/pwsh"
  rm -rf "$dir"
  mv "$tmp/pwsh" "$dir"
fi
ln -sfn "$dir/pwsh" /usr/local/bin/pwsh
"#;

/// GNU bash 3.2.57, the bash macOS ships as /bin/bash, for the one test that needs that bash on Linux: the spare hold's SIGWINCH negative control (`a_spare_hold_survives_sigwinch_and_only_go_releases_it`), whose cut hold a signal releases only where the hold's FIFO open fails when interrupted. bash 5 (the guest's 5.2) restarts it, so without this bash the control is COULD NOT RUN and the shell suite's row fails.
pub const BASH_3_2: (&str, &str) = (
    "3.2.57",
    "3fa9daf85ebf35068f090ce51283ddeeb3c75eb5bc70b1a4a7cb05868bfe06a4",
);

/// Build [`BASH_3_2`] (`$1` its version, `$2` the archive's sha256) into `/opt/aterm-lane/bash-<version>` and link it as `/usr/local/bin/bash-3.2`, run as the guest's user (root only to install).
pub const BASH_3_2_INSTALL: &str = r#"set -eu
dir=/opt/aterm-lane/bash-$1
if [ ! -x "$dir/bin/bash" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL -o "$tmp/bash.tar.gz" "https://ftp.gnu.org/gnu/bash/bash-$1.tar.gz"
  echo "$2  $tmp/bash.tar.gz" | sha256sum -c --quiet
  tar -xzf "$tmp/bash.tar.gz" -C "$tmp"
  cd "$tmp/bash-$1"
  cp /usr/share/misc/config.guess /usr/share/misc/config.sub support/
  ./configure --prefix="$dir" --without-bash-malloc >"$tmp/build.log" 2>&1 &&
    make >>"$tmp/build.log" 2>&1 || { tail -40 "$tmp/build.log"; exit 1; }
  sudo install -D -m 0755 bash "$dir/bin/.bash.new"
  sudo mv "$dir/bin/.bash.new" "$dir/bin/bash"
fi
sudo ln -sfn "$dir/bin/bash" /usr/local/bin/bash-3.2
"#;

/// Webster's Second International (`web2`), the public-domain word list aterm-lexicon's dictionary gates walk: their allowlists are curated against it, so another list decides nothing and they answer COULD NOT RUN without it (`crates/aterm-lexicon/tests/lexicon_suite.rs`). macOS ships it here and Ubuntu packages no web2 (its `wamerican` is another list), so the guest gets this Mac's copy at the same path.
pub const WEB2: &str = aterm_uds::os_path!("web2");

/// One provisioning step: an argv run in the guest, with an optional stdin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub label: &'static str,
    pub argv: Vec<String>,
    pub stdin: Option<String>,
}

fn step(label: &'static str, argv: &[&str], stdin: Option<&str>) -> Step {
    Step {
        label,
        argv: argv.iter().map(|s| (*s).to_string()).collect(),
        stdin: stdin.map(str::to_string),
    }
}

/// The provisioning plan, in order, with this Mac's [`WEB2`] when it reads ([`provision_plan`]).
#[must_use]
pub fn provision_steps() -> Vec<Step> {
    provision_plan(std::fs::read_to_string(WEB2).ok())
}

/// The provisioning plan, in order, laying `web2` at [`WEB2`] when it is given.
#[must_use]
pub fn provision_plan(web2: Option<String>) -> Vec<Step> {
    let mut install = vec![
        "sudo",
        "env",
        "DEBIAN_FRONTEND=noninteractive",
        "apt-get",
        "install",
        "-y",
        "-q",
        "--no-install-recommends",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    install.extend(packages());
    let mut steps = vec![
        step(
            "arm64 sources",
            &["sudo", "tee", "/etc/apt/sources.list.d/ubuntu.sources"],
            Some(SOURCES_ARM64),
        ),
        step(
            "amd64 sources",
            &[
                "sudo",
                "tee",
                "/etc/apt/sources.list.d/ubuntu-amd64.sources",
            ],
            Some(SOURCES_AMD64),
        ),
        step(
            "amd64 architecture",
            &["sudo", "dpkg", "--add-architecture", "amd64"],
            None,
        ),
        // A VM stopped mid-install (the Mac restarted under it) leaves dpkg interrupted, and
        // apt refuses every install until its unpacked packages are configured; with nothing
        // pending this does nothing.
        step(
            "dpkg configure",
            &[
                "sudo",
                "env",
                "DEBIAN_FRONTEND=noninteractive",
                "dpkg",
                "--configure",
                "-a",
            ],
            None,
        ),
        step("apt-get update", &["sudo", "apt-get", "update", "-q"], None),
        Step {
            label: "apt-get install",
            argv: install,
            stdin: None,
        },
        step(
            "rust toolchain",
            &[
                "rustup",
                "toolchain",
                "install",
                RUST,
                "--profile",
                "minimal",
                "--target",
                Arch::X86_64.triple(),
            ],
            None,
        ),
        step(
            "pwsh",
            &["sudo", "sh", "-s", "--", PWSH.0, PWSH.1],
            Some(PWSH_INSTALL),
        ),
        step(
            "bash 3.2",
            &["sh", "-s", "--", BASH_3_2.0, BASH_3_2.1],
            Some(BASH_3_2_INSTALL),
        ),
    ];
    if let Some(words) = web2 {
        steps.push(step(
            "web2",
            &["sudo", "install", "-D", "-m", "0644", "/dev/stdin", WEB2],
            Some(&words),
        ));
    }
    steps
}

/// The plan's fingerprint (FNV-1a over every step), which [`STAMP`] holds once it ran: a changed plan provisions again.
#[must_use]
pub fn fingerprint(steps: &[Step]) -> String {
    let mut text = String::new();
    for s in steps {
        let _ = writeln!(
            text,
            "{}\x1f{}",
            s.argv.join("\x1f"),
            s.stdin.as_deref().unwrap_or("")
        );
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The environment `cargo` gets for `arch`: the pinned stock toolchain, no incremental cache, the guest-local target dir, and for x86_64 the cross linker, C compiler and pkg-config search path (`CARGO_BUILD_TARGET` names x86_64, a cross triple here, only when `default_target` asks) and libtest's one-test-at-a-time (`RUST_TEST_THREADS=1`, which a CMD's own `--test-threads` overrides): the proof's rule for Rosetta ([`crate::linux_proof::ROSETTA_ONE_AT_A_TIME`]) for every x86_64 `exec`.
#[must_use]
pub fn cargo_env(arch: Arch, target_dir: &str, default_target: bool) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("RUSTUP_TOOLCHAIN".into(), RUST.into()),
        ("CARGO_INCREMENTAL".into(), "0".into()),
        ("CARGO_TARGET_DIR".into(), target_dir.into()),
        ("CARGO_TERM_COLOR".into(), "never".into()),
    ];
    if arch == Arch::X86_64 {
        let t = arch.triple().replace('-', "_");
        env.extend([
            (
                format!("CARGO_TARGET_{}_LINKER", t.to_uppercase()),
                "x86_64-linux-gnu-gcc".into(),
            ),
            (format!("CC_{t}"), "x86_64-linux-gnu-gcc".into()),
            (format!("AR_{t}"), "x86_64-linux-gnu-ar".into()),
            ("PKG_CONFIG_ALLOW_CROSS".into(), "1".into()),
            ("RUST_TEST_THREADS".into(), "1".into()),
            (
                format!("PKG_CONFIG_LIBDIR_{t}"),
                "/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig".into(),
            ),
        ]);
        if default_target {
            env.push(("CARGO_BUILD_TARGET".into(), arch.triple().into()));
        }
    }
    env
}

/// `env K=V… ARGV…`: the argv that runs `argv` with `env` added.
#[must_use]
pub fn with_env(env: &[(String, String)], argv: &[String]) -> Vec<String> {
    let mut out = vec!["env".to_string()];
    out.extend(env.iter().map(|(k, v)| format!("{k}={v}")));
    out.extend(argv.iter().cloned());
    out
}

/// A display server the lane runs a client under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    /// X11: Xvfb.
    Xvfb,
    /// Wayland: weston's headless backend.
    Weston,
    /// Wayland: Mutter in headless mode, GNOME's compositor (client-side decorations).
    Mutter,
    /// X11 on Wayland: a rootful Xwayland inside headless weston.
    Xwayland,
    /// Wayland: weston's x11 backend, its output a window on the lane's Xvfb, whose seat (a pointer and a keyboard) reads that window's input, so XTEST on the Xvfb display drives it (`lane-bus xseat`).
    WestonX11,
}

/// How the baseline gives a Wayland screen's seat its keys and pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seat {
    /// A RemoteDesktop session of Mutter's (`lane-bus seat`), whose virtual devices live as long as the session.
    RemoteDesktop,
    /// XTEST on the X display weston's x11 backend reads its input from (`lane-bus xseat`): weston's own keyboard and pointer.
    Xtest,
}

/// A user unit the lane starts in the guest (`systemd-run --user`), and the socket that says it is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub env: Vec<(String, String)>,
    pub argv: Vec<String>,
    pub socket: String,
}

/// Every unit the lane starts is named with this prefix, and a teardown stops them all by it.
pub const UNIT_PREFIX: &str = "aterm-lane-";

/// The X display numbers the lane takes: high, so a person's own session
/// never holds them.
const XVFB_DISPLAY: &str = ":91";
const XWAYLAND_DISPLAY: &str = ":92";

/// The virtual screen every server gets.
const WIDTH: &str = "1280";
const HEIGHT: &str = "800";

/// Every baseline screen with a seat in its seat's pixels (Mutter's logical ones; the fractional Mutter's 1920x1200 at 1.5): [`WIDTH`] by [`HEIGHT`].
pub const SCREEN: (i32, i32) = (1280, 800);

impl Display {
    pub const ALL: [Display; 5] = [
        Display::Xvfb,
        Display::Weston,
        Display::Mutter,
        Display::Xwayland,
        Display::WestonX11,
    ];

    /// The displays a proof or a CPU run takes when none is named: every one but [`Display::WestonX11`], a second weston that only the baseline's input items need.
    pub const PROOF: [Display; 4] = [
        Display::Xvfb,
        Display::Weston,
        Display::Mutter,
        Display::Xwayland,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Display::Xvfb => "xvfb",
            Display::Weston => "weston",
            Display::Mutter => "mutter",
            Display::Xwayland => "xwayland",
            Display::WestonX11 => "weston-x11",
        }
    }

    /// The protocol a client speaks to it.
    #[must_use]
    pub const fn protocol(self) -> &'static str {
        match self {
            Display::Xvfb | Display::Xwayland => "X11",
            Display::Weston | Display::Mutter | Display::WestonX11 => "Wayland",
        }
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Display> {
        Display::ALL.into_iter().find(|d| d.name() == word)
    }

    /// The proof display under which a proof replays this display's baseline screens: weston for weston's x11 backend (the same weston, its seat read from an Xvfb window), the display itself otherwise.
    #[must_use]
    pub const fn proof_display(self) -> Display {
        match self {
            Display::WestonX11 => Display::Weston,
            d => d,
        }
    }

    /// How the baseline drives its seat: `None` on X11 (XTEST reaches the client's own window) and under headless weston, which advertises no `wl_seat`.
    #[must_use]
    pub const fn seat(self) -> Option<Seat> {
        match self {
            Display::Mutter => Some(Seat::RemoteDesktop),
            Display::WestonX11 => Some(Seat::Xtest),
            Display::Xvfb | Display::Weston | Display::Xwayland => None,
        }
    }

    /// The X display XTEST drives this display's input through: the client's own on X11, the one weston's x11 backend is nested in, none under a headless compositor.
    #[must_use]
    pub const fn input_x(self) -> Option<&'static str> {
        match self {
            Display::Xvfb | Display::WestonX11 => Some(XVFB_DISPLAY),
            Display::Xwayland => Some(XWAYLAND_DISPLAY),
            Display::Weston | Display::Mutter => None,
        }
    }

    /// Whether a client with no input device can own the clipboard and read it back.
    #[must_use]
    pub const fn clipboard_scriptable(self) -> bool {
        matches!(self, Display::Xvfb | Display::Xwayland)
    }

    /// Whether its servers include a weston, which reads [`Config::weston_ini`].
    #[must_use]
    pub const fn runs_weston(self) -> bool {
        matches!(
            self,
            Display::Weston | Display::Xwayland | Display::WestonX11
        )
    }

    /// The servers it needs, in start order.
    #[must_use]
    pub fn units(self, runtime: &str, config: &Config) -> Vec<Unit> {
        let weston_ini = config.weston_ini();
        let weston = || Unit {
            name: format!("{UNIT_PREFIX}weston"),
            env: Vec::new(),
            argv: strings(&[
                "weston",
                "--backend=headless",
                "--renderer=gl",
                &format!("--socket={UNIT_PREFIX}weston"),
                &format!("--width={WIDTH}"),
                &format!("--height={HEIGHT}"),
                "--idle-time=0",
                &format!("--config={weston_ini}"),
            ]),
            socket: format!("{runtime}/{UNIT_PREFIX}weston"),
        };
        let xvfb = || Unit {
            name: format!("{UNIT_PREFIX}xvfb"),
            env: Vec::new(),
            argv: strings(&[
                "Xvfb",
                XVFB_DISPLAY,
                "-screen",
                "0",
                &format!("{WIDTH}x{HEIGHT}x24"),
                "-nolisten",
                "tcp",
                "-noreset",
            ]),
            socket: x_socket(XVFB_DISPLAY),
        };
        match self {
            Display::Xvfb => vec![xvfb()],
            Display::Weston => vec![weston()],
            Display::WestonX11 => vec![
                xvfb(),
                Unit {
                    name: format!("{UNIT_PREFIX}weston-x11"),
                    env: vec![("DISPLAY".into(), XVFB_DISPLAY.into())],
                    argv: strings(&[
                        "weston",
                        "--backend=x11",
                        "--renderer=gl",
                        &format!("--socket={UNIT_PREFIX}weston-x11"),
                        &format!("--width={WIDTH}"),
                        &format!("--height={HEIGHT}"),
                        "--idle-time=0",
                        &format!("--config={weston_ini}"),
                        "--debug",
                    ]),
                    socket: format!("{runtime}/{UNIT_PREFIX}weston-x11"),
                },
            ],
            Display::Mutter => vec![Unit {
                name: format!("{UNIT_PREFIX}mutter"),
                env: vec![("XDG_SESSION_TYPE".into(), "wayland".into())],
                argv: strings(&[
                    "mutter",
                    "--headless",
                    "--wayland",
                    "--no-x11",
                    "--virtual-monitor",
                    &format!("{WIDTH}x{HEIGHT}"),
                    "--wayland-display",
                    &format!("{UNIT_PREFIX}mutter"),
                ]),
                socket: format!("{runtime}/{UNIT_PREFIX}mutter"),
            }],
            Display::Xwayland => vec![
                weston(),
                Unit {
                    name: format!("{UNIT_PREFIX}xwayland"),
                    env: vec![(
                        "WAYLAND_DISPLAY".into(),
                        format!("{runtime}/{UNIT_PREFIX}weston"),
                    )],
                    argv: strings(&[
                        "Xwayland",
                        XWAYLAND_DISPLAY,
                        "-noreset",
                        "-nolisten",
                        "tcp",
                        "-geometry",
                        &format!("{WIDTH}x{HEIGHT}"),
                    ]),
                    socket: x_socket(XWAYLAND_DISPLAY),
                },
            ],
        }
    }

    /// What a client is told to reach it by: an X display, or the Wayland socket's absolute path (which libwayland takes as given, whatever the client's own `XDG_RUNTIME_DIR` is).
    #[must_use]
    pub fn client_env(self, runtime: &str) -> (String, String) {
        match self {
            Display::Xvfb => ("DISPLAY".into(), XVFB_DISPLAY.into()),
            Display::Xwayland => ("DISPLAY".into(), XWAYLAND_DISPLAY.into()),
            Display::Weston | Display::Mutter | Display::WestonX11 => (
                "WAYLAND_DISPLAY".into(),
                format!("{runtime}/{UNIT_PREFIX}{}", self.name()),
            ),
        }
    }
}

/// The scale a [`Screen`]'s output carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// The server's own: 1.
    One,
    /// X11's: `Xft.dpi: 144` in the root window's RESOURCE_MANAGER (what a desktop's font-scaling setting writes), which the winit fork reads through aterm-x11's resource database (x11rb's before LW.7) as a scale of 1.5.
    XftDpi144,
    /// weston's headless output at `--scale=2`.
    Two,
    /// Mutter's virtual monitor at 1920x1200 with a logical scale of 1.5, sent to the client through `wp_fractional_scale_v1`.
    OneAndHalf,
}

/// [`Config::weston_ini`]'s text: desktop-shell's startup fade off.
pub const WESTON_INI_TEXT: &str = "[shell]\nstartup-animation=none\n";

/// The GSettings keyfile that keeps ibus off the X keymap.
pub const IBUS_KEYFILE: &str = "[desktop/ibus/general]\nuse-system-keyboard-layout=true\n";

/// The GSettings keyfile that turns on Mutter's fractional scaling.
pub const MUTTER_KEYFILE: &str =
    "[org/gnome/mutter]\nexperimental-features=['scale-monitor-framebuffer']\n";

/// `xkb/symbols/us` under a baseline Mutter's `XDG_CONFIG_HOME`: the default layout, `us`, made `us(intl)`.
pub const MUTTER_XKB_US: &str = "default partial alphanumeric_keys modifier_keys\n\
xkb_symbols \"lane\" {\n    include \"us(intl)\"\n};\n";

/// GNOME Shell's settings in the baseline (the input-method item runs it headless on its screen's `XDG_CONFIG_HOME`, through a GSettings keyfile): one input source, `us+intl`, which GNOME Shell sets as Mutter's keymap and serves through IBus's `xkb:us::eng` engine, ibus's simple engine, which composes dead keys; and no welcome dialog over the first window.
pub const SHELL_KEYFILE: &str = "[org/gnome/desktop/input-sources]\n\
sources=[('xkb', 'us+intl')]\n\
[org/gnome/shell]\n\
welcome-dialog-last-shown-version='999'\n";

/// The `monitors.xml` that puts Mutter's one virtual monitor (connector `Meta-0`, as Mutter 46 names it) at 1920x1200, scale 1.5: a logical 1280x800, the size of every other screen.
pub const MUTTER_MONITORS: &str = "\
<monitors version=\"2\">
  <configuration>
    <layoutmode>logical</layoutmode>
    <logicalmonitor>
      <x>0</x>
      <y>0</y>
      <scale>1.5</scale>
      <primary>yes</primary>
      <monitor>
        <monitorspec>
          <connector>Meta-0</connector>
          <vendor>MetaVendor</vendor>
          <product>MetaVirtualMonitor</product>
          <serial>0x00</serial>
        </monitorspec>
        <mode>
          <width>1920</width>
          <height>1200</height>
          <rate>60.000</rate>
        </mode>
      </monitor>
    </logicalmonitor>
  </configuration>
</monitors>
";

/// A display as the baseline runs it: a server, and the scale its output carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub display: Display,
    pub scale: Scale,
}

impl Screen {
    /// Every screen the baseline records, in the order it takes them.
    pub const BASELINE: [Screen; 8] = [
        Screen::plain(Display::Xvfb),
        Screen {
            display: Display::Xvfb,
            scale: Scale::XftDpi144,
        },
        Screen::plain(Display::Weston),
        Screen {
            display: Display::Weston,
            scale: Scale::Two,
        },
        Screen::plain(Display::Mutter),
        Screen {
            display: Display::Mutter,
            scale: Scale::OneAndHalf,
        },
        Screen::plain(Display::Xwayland),
        Screen::plain(Display::WestonX11),
    ];

    /// The screens the stats Deck's layout rows run under: one per display family, at its server's own scale (`linux_baseline::deck_row`).
    pub const DECK: [Screen; 5] = [
        Screen::plain(Display::Xvfb),
        Screen::plain(Display::Weston),
        Screen::plain(Display::Mutter),
        Screen::plain(Display::Xwayland),
        Screen::plain(Display::WestonX11),
    ];

    /// `display` at its server's own scale.
    #[must_use]
    pub const fn plain(display: Display) -> Screen {
        Screen {
            display,
            scale: Scale::One,
        }
    }

    /// Its name: the display's, with the scale when it is not 1.
    #[must_use]
    pub fn name(self) -> String {
        let suffix = match self.scale {
            Scale::One => "",
            Scale::XftDpi144 => "-dpi144",
            Scale::Two => "-scale2",
            Scale::OneAndHalf => "-scale1.5",
        };
        format!("{}{suffix}", self.display.name())
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Screen> {
        Screen::BASELINE.into_iter().find(|s| s.name() == word)
    }

    /// Its servers, in start order.
    #[must_use]
    pub fn units(self, runtime: &str, config: &Config) -> Vec<Unit> {
        let mut units = self.display.units(runtime, config);
        match self.scale {
            Scale::One | Scale::XftDpi144 => {}
            Scale::Two => {
                for u in &mut units {
                    u.argv.push("--scale=2".into());
                }
            }
            Scale::OneAndHalf => {
                for u in &mut units {
                    for a in &mut u.argv {
                        if a == &format!("{WIDTH}x{HEIGHT}") {
                            *a = "1920x1200".into();
                        }
                    }
                    u.env.push(("GSETTINGS_BACKEND".into(), "keyfile".into()));
                    u.env.push(("XDG_CONFIG_HOME".into(), config.mutter()));
                }
            }
        }
        units
    }

    /// The files its servers read, written before they start: `(path, text)`.
    #[must_use]
    pub fn files(self, config: &Config) -> Vec<(String, String)> {
        let mut files = match self.scale {
            Scale::OneAndHalf => vec![
                (
                    format!("{}/glib-2.0/settings/keyfile", config.mutter()),
                    MUTTER_KEYFILE.into(),
                ),
                (
                    format!("{}/monitors.xml", config.mutter()),
                    MUTTER_MONITORS.into(),
                ),
            ],
            _ => Vec::new(),
        };
        if self.display.runs_weston() {
            files.push((config.weston_ini(), WESTON_INI_TEXT.into()));
        }
        files
    }

    /// What runs once its servers are up, before a client starts.
    #[must_use]
    pub fn after(self) -> Vec<Step> {
        if self.scale != Scale::XftDpi144 {
            return Vec::new();
        }
        let display = format!("DISPLAY={}", self.display.client_env("").1);
        vec![Step {
            label: "Xft.dpi 144",
            argv: strings(&["env", &display, "xrdb", "-merge"]),
            stdin: Some("Xft.dpi: 144\n".into()),
        }]
    }

    /// The scratch `XDG_CONFIG_HOME` of the screen's Mutter in the baseline (`None`: the screen runs no Mutter).
    #[must_use]
    pub fn baseline_config(self, config: &Config) -> Option<String> {
        match (self.display, self.scale) {
            (Display::Mutter, Scale::OneAndHalf) => Some(config.mutter()),
            (Display::Mutter, _) => Some(config.mutter_plain()),
            _ => None,
        }
    }

    /// The servers the baseline starts: [`Screen::units`], a Mutter's on its [`Screen::baseline_config`], whose keyboard data gives the keymap a dead key ([`MUTTER_XKB_US`]).
    #[must_use]
    pub fn baseline_units(self, runtime: &str, config: &Config) -> Vec<Unit> {
        let mut units = self.units(runtime, config);
        if let Some(config) = self.baseline_config(config) {
            for u in &mut units {
                u.env.retain(|(k, _)| k != "XDG_CONFIG_HOME");
                u.env.push(("XDG_CONFIG_HOME".into(), config.clone()));
            }
        }
        units
    }

    /// The files the baseline's servers read, written before they start: [`Screen::files`], and under a Mutter's [`Screen::baseline_config`] its keyboard data and GNOME Shell's settings (one keyfile with the fractional scale's).
    #[must_use]
    pub fn baseline_files(self, config: &Config) -> Vec<(String, String)> {
        let Some(mutter_config) = self.baseline_config(config) else {
            return self.files(config);
        };
        let keyfile = format!("{mutter_config}/glib-2.0/settings/keyfile");
        let mut files: Vec<(String, String)> = self
            .files(config)
            .into_iter()
            .filter(|(path, _)| path != &keyfile)
            .collect();
        let scaled = if self.scale == Scale::OneAndHalf {
            MUTTER_KEYFILE
        } else {
            ""
        };
        files.push((keyfile, format!("{scaled}{SHELL_KEYFILE}")));
        files.push((
            format!("{mutter_config}/xkb/symbols/us"),
            MUTTER_XKB_US.into(),
        ));
        files
    }

    /// GNOME Shell, headless, on the screen's monitor and its Mutter's [`Screen::baseline_config`] (GSettings from its keyfile), on a socket of its own: the compositor of the baseline's input-method item.
    #[must_use]
    pub fn gnome_shell(self, runtime: &str, config: &Config) -> Option<Unit> {
        self.baseline_config(config)?;
        let mut unit = self.baseline_units(runtime, config).into_iter().next()?;
        let name = format!("{UNIT_PREFIX}gnome-shell");
        unit.argv[0] = "gnome-shell".into();
        for a in &mut unit.argv {
            if a == &format!("{UNIT_PREFIX}mutter") {
                a.clone_from(&name);
            }
        }
        unit.env.retain(|(k, _)| k != "GSETTINGS_BACKEND");
        unit.env
            .push(("GSETTINGS_BACKEND".into(), "keyfile".into()));
        unit.socket = format!("{runtime}/{name}");
        unit.name = name;
        Some(unit)
    }

    /// The baseline's X keymap, `us intl`, and the socket of the X server it is set on: set once that server listens, before the servers after it and the client start (an X display's only: an X11 screen's, and the Xvfb weston's x11 backend is nested in, which builds its own keymap from the X server's rule names once, as it starts; the proof's rows and `exec` keep the server's own `us`, where an apostrophe typed through a real key stays one).
    #[must_use]
    pub fn baseline_keymap(self) -> Option<(String, Step)> {
        let display = self.display.input_x()?;
        let step = Step {
            label: "keymap us intl",
            argv: strings(&[
                "env",
                &format!("DISPLAY={display}"),
                "setxkbmap",
                "-layout",
                "us",
                "-variant",
                "intl",
            ]),
            stdin: None,
        };
        Some((x_socket(display), step))
    }
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|s| (*s).to_string()).collect()
}

#[must_use]
pub fn x_socket(display: &str) -> String {
    format!("/tmp/.X11-unix/X{}", display.trim_start_matches(':'))
}

/// `systemd-run --user … -- ARGV`: `unit` started as a transient user unit, collected when it ends, so the guest keeps nothing of a run.
#[must_use]
pub fn run_unit(unit: &Unit, workdir: Option<&str>) -> Vec<String> {
    let mut argv = strings(&["systemd-run", "--user", "--quiet", "--collect"]);
    argv.push(format!("--unit={}", unit.name));
    if let Some(dir) = workdir {
        argv.push(format!("--working-directory={dir}"));
    }
    argv.extend(unit.env.iter().map(|(k, v)| format!("--setenv={k}={v}")));
    argv.push("--".into());
    argv.extend(unit.argv.iter().cloned());
    argv
}

/// Wait, in the guest, until `socket` exists: `timeout` ends the wait (exit 124) after `secs`, a hang detector.
#[must_use]
pub fn wait_socket(socket: &str, secs: u32) -> Vec<String> {
    vec![
        "timeout".into(),
        secs.to_string(),
        "sh".into(),
        "-c".into(),
        "until [ -S \"$1\" ]; do sleep 0.1; done".into(),
        "wait-socket".into(),
        socket.into(),
    ]
}

/// Wait, in the guest, until the file `path` exists: a hang detector, as [`wait_socket`].
#[must_use]
pub fn wait_file(path: &str, secs: u32) -> Vec<String> {
    vec![
        "timeout".into(),
        secs.to_string(),
        "sh".into(),
        "-c".into(),
        "until [ -e \"$1\" ]; do sleep 0.1; done".into(),
        "wait-file".into(),
        path.into(),
    ]
}

/// Wait, in the guest, until the file `path` holds at least `n` lines that contain `text`: a hang detector, as [`wait_socket`].
#[must_use]
pub fn wait_lines(path: &str, text: &str, n: usize, secs: u32) -> Vec<String> {
    vec![
        "timeout".into(),
        secs.to_string(),
        "sh".into(),
        "-c".into(),
        "until [ \"$(grep -cF -- \"$2\" \"$1\" 2>/dev/null)\" -ge \"$3\" ] 2>/dev/null; \
         do sleep 0.1; done"
            .into(),
        "wait-lines".into(),
        path.into(),
        text.into(),
        n.to_string(),
    ]
}

/// Wait, in the guest, until a process owns the bus name `name` on the session bus (an activatable name nobody runs does not count): a hang detector, as [`wait_socket`].
#[must_use]
pub fn wait_name(name: &str, secs: u32) -> Vec<String> {
    vec![
        "timeout".into(),
        secs.to_string(),
        "sh".into(),
        "-c".into(),
        "until busctl --user list --no-legend | awk -v n=\"$1\" '$1 == n && $2 ~ /^[0-9]+$/ { f = 1 } \
         END { exit !f }'; do sleep 0.1; done"
            .into(),
        "wait-name".into(),
        name.into(),
    ]
}

/// Send `signal` to the main process of the user unit `name`: SIGKILL ends a server as a crash does, with no goodbye to its clients, and SIGSTOP freezes it.
#[must_use]
pub fn signal_unit(name: &str, signal: &str) -> Vec<String> {
    vec![
        "systemctl".into(),
        "--user".into(),
        "kill".into(),
        "--kill-whom=main".into(),
        format!("--signal={signal}"),
        name.into(),
    ]
}

/// Wait, in the guest, until a socket of the user unit `name`'s main process holds bytes it has not read (`ss`'s Recv-Q): what a client sent a frozen server.
#[must_use]
pub fn wait_unread(name: &str, secs: u32) -> Vec<String> {
    vec![
        "timeout".into(),
        secs.to_string(),
        "sh".into(),
        "-c".into(),
        "pid=$(systemctl --user show --property=MainPID --value \"$1\") && \
         until ss -xpnH | grep -F \"pid=$pid,\" | awk '$3 > 0 { f = 1 } END { exit !f }'; \
         do sleep 0.1; done"
            .into(),
        "wait-unread".into(),
        name.into(),
    ]
}

/// Print, in the guest, the bytes the user unit `name`'s main process holds unread on its sockets (`ss`'s Recv-Q, summed), as `<n> bytes`.
#[must_use]
pub fn unread_bytes(name: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        "pid=$(systemctl --user show --property=MainPID --value \"$1\") && \
         ss -xpnH | grep -F \"pid=$pid,\" | awk '{ n += $3 } END { print n + 0 \" bytes\" }'"
            .into(),
        "unread-bytes".into(),
        name.into(),
    ]
}

/// Wait, in the guest, until the user unit `name` is no longer active: a hang detector, as [`wait_socket`].
#[must_use]
pub fn wait_unit_gone(name: &str, secs: u32) -> Vec<String> {
    vec![
        "timeout".into(),
        secs.to_string(),
        "sh".into(),
        "-c".into(),
        "while systemctl --user is-active --quiet \"$1\"; do sleep 0.1; done".into(),
        "wait-unit".into(),
        name.into(),
    ]
}

/// An X selection a second client (xclip) reads or owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// CLIPBOARD: what copy and paste carry.
    Clipboard,
    /// PRIMARY: what a selection made with the mouse offers, and a middle click pastes.
    Primary,
}

impl Selection {
    /// xclip's name for it (`-selection`).
    #[must_use]
    pub const fn xclip(self) -> &'static str {
        match self {
            Selection::Clipboard => "clipboard",
            Selection::Primary => "primary",
        }
    }
}

/// What a second X client (xclip) reads as `sel` on the X `display`, on a line of its own: the X server asks the owner by `ConvertSelection` and carries its answer.
#[must_use]
pub fn read_clipboard(display: &str, sel: Selection, secs: u32) -> Vec<String> {
    vec![
        "env".into(),
        format!("DISPLAY={display}"),
        "timeout".into(),
        secs.to_string(),
        "sh".into(),
        "-c".into(),
        "xclip -o -selection \"$1\" 2>/dev/null && echo".into(),
        "read-clipboard".into(),
        sel.xclip().into(),
    ]
}

/// xclip owning `sel` on the X `display` with `text`, as a lane unit: `-quiet` keeps it in the foreground, serving `text` until another client takes the selection or [`stop_units`] stops it.
#[must_use]
pub fn clipboard_owner(display: &str, sel: Selection, text: &str) -> Unit {
    Unit {
        name: format!("{UNIT_PREFIX}xclip"),
        env: vec![("DISPLAY".into(), display.into())],
        argv: vec![
            "sh".into(),
            "-c".into(),
            "printf %s \"$1\" | xclip -i -selection \"$2\" -quiet".into(),
            "clipboard-owner".into(),
            text.into(),
            sel.xclip().into(),
        ],
        socket: String::new(),
    }
}

/// Wait, in the guest, until a reader of the X `display`'s `sel` gets `text`: the owner [`clipboard_owner`] started has taken the selection.
#[must_use]
pub fn wait_clipboard(display: &str, sel: Selection, text: &str, secs: u32) -> Vec<String> {
    vec![
        "env".into(),
        format!("DISPLAY={display}"),
        "timeout".into(),
        secs.to_string(),
        "sh".into(),
        "-c".into(),
        "until [ \"$(xclip -o -selection \"$2\" 2>/dev/null)\" = \"$1\" ]; do sleep 0.1; done"
            .into(),
        "wait-clipboard".into(),
        text.into(),
        sel.xclip().into(),
    ]
}

/// Stop every unit the lane started, then forget any that failed.
#[must_use]
pub fn stop_units() -> [Vec<String>; 2] {
    let pattern = format!("{UNIT_PREFIX}*");
    [
        strings(&["systemctl", "--user", "stop", &pattern]),
        strings(&["systemctl", "--user", "reset-failed", &pattern]),
    ]
}

/// Where the guest kernel shows Rosetta's binfmt registration, which lima makes at boot (`vmOpts.vz.rosetta.binfmt`): without it no x86_64 binary runs.
pub const ROSETTA_BINFMT: &str = "/proc/sys/fs/binfmt_misc/rosetta";

/// Whether [`ROSETTA_BINFMT`]'s text is an enabled registration of an interpreter.
#[must_use]
pub fn rosetta_registered(text: &str) -> bool {
    let mut lines = text.lines();
    lines.next() == Some("enabled") && lines.any(|l| l.starts_with("interpreter /"))
}

/// Rosetta's AOT cache in the guest, which lima's `rosettad` keeps.
pub const ROSETTA_CACHE: &str = "/var/cache/rosettad";

/// The stamp: the boot ([`BOOT_ID`]) the cache was emptied in.
pub const ROSETTA_CACHE_BOOT: &str = "/var/cache/rosettad/aterm-lane-boot-id";

/// The guest kernel's id of this boot.
pub const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

/// Empty Rosetta's AOT cache and stamp it with `boot_id`; `rosettad` is stopped around the removal so it writes nothing half-way.
#[must_use]
pub fn rosetta_cache_reset(boot_id: &str) -> Vec<Step> {
    vec![
        step(
            "stop rosettad",
            &["sudo", "systemctl", "stop", "rosettad"],
            None,
        ),
        step(
            "empty the AOT cache",
            &[
                "sudo",
                "find",
                ROSETTA_CACHE,
                "-name",
                "*.aotcache",
                "-delete",
            ],
            None,
        ),
        step(
            "stamp the boot",
            &["sudo", "tee", ROSETTA_CACHE_BOOT],
            Some(&format!("{}\n", boot_id.trim())),
        ),
        step(
            "start rosettad",
            &["sudo", "systemctl", "start", "rosettad"],
            None,
        ),
    ]
}

/// Ask the session bus for the accessibility bus (`org.a11y.Bus.GetAddress`), which D-Bus activates at-spi2-core's bus launcher when it is not up yet.
#[must_use]
pub fn a11y_address() -> Vec<String> {
    strings(&[
        "busctl",
        "--user",
        "call",
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.a11y.Bus",
        "GetAddress",
    ])
}

/// The address in `busctl`'s answer (`s "unix:path=…"`), if it is one.
#[must_use]
pub fn parse_a11y_address(reply: &str) -> Option<&str> {
    let rest = reply.trim().strip_prefix("s \"")?.strip_suffix('"')?;
    rest.starts_with("unix:").then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arches_name_their_triples_and_targets() {
        assert_eq!(Arch::parse("arm64"), Some(Arch::Aarch64));
        assert_eq!(Arch::parse("amd64"), Some(Arch::X86_64));
        assert_eq!(Arch::parse("riscv64"), None);
        assert!(Arch::Aarch64.target_args().is_empty());
        assert_eq!(
            Arch::X86_64.target_args(),
            ["--target", "x86_64-unknown-linux-gnu"]
        );
        assert_eq!(Arch::Aarch64.bin_dir("/t"), "/t/debug");
        assert_eq!(
            Arch::X86_64.bin_dir("/t"),
            "/t/x86_64-unknown-linux-gnu/debug"
        );
    }

    #[test]
    fn every_library_is_installed_for_both_architectures() {
        let all = packages();
        for lib in DEV_LIBRARIES.iter().chain(MESA) {
            assert!(all.iter().any(|p| p == lib), "{lib}");
            assert!(
                all.iter().any(|p| *p == format!("{lib}:amd64")),
                "{lib}:amd64"
            );
        }
        for tool in TOOLS {
            assert!(
                !all.contains(&format!("{tool}:amd64")),
                "{tool} is the guest's own"
            );
        }
        for lib in BROWSER_AMD64 {
            assert!(all.contains(&format!("{lib}:amd64")), "{lib}:amd64");
        }
        for lib in BROWSER_ARM64 {
            assert!(all.iter().any(|p| p == lib), "{lib}");
        }
        let first_foreign = all.iter().position(|p| p.ends_with(":amd64"));
        assert!(
            all[first_foreign.expect("amd64 rows")..]
                .iter()
                .all(|p| p.ends_with(":amd64"))
        );
    }

    #[test]
    fn each_source_names_one_architecture() {
        for (text, arch) in [(SOURCES_ARM64, "arm64"), (SOURCES_AMD64, "amd64")] {
            let stanzas = text.split("\n\n").count();
            assert_eq!(
                text.matches(&format!("Architectures: {arch}\n")).count(),
                stanzas
            );
        }
    }

    #[test]
    fn the_fingerprint_moves_with_the_plan() {
        let plan = provision_plan(None);
        assert_eq!(fingerprint(&plan), fingerprint(&provision_plan(None)));
        assert_eq!(fingerprint(&plan).len(), 16);
        let mut changed = plan.clone();
        let install = plan.iter().position(|s| s.label == "apt-get install");
        changed[install.expect("an install step")]
            .argv
            .push("extra-package".into());
        assert_ne!(fingerprint(&plan), fingerprint(&changed));
        let mut sources = plan.clone();
        sources[0].stdin = Some(SOURCES_AMD64.into());
        assert_ne!(fingerprint(&plan), fingerprint(&sources));
        let words = provision_plan(Some("a\nb\n".into()));
        assert_ne!(fingerprint(&plan), fingerprint(&words));
        assert_ne!(
            fingerprint(&words),
            fingerprint(&provision_plan(Some("a\nc\n".into())))
        );
    }

    /// The host's web2 goes to the guest at its own path, its words as the step's
    /// input; a host without one lays nothing there, and those gates say so.
    #[test]
    fn the_guest_gets_the_hosts_web2_at_its_own_path() {
        let plan = provision_plan(Some("aa\naal\n".into()));
        let web2 = plan.last().expect("a plan");
        assert_eq!(web2.label, "web2");
        assert_eq!(web2.argv.last().map(String::as_str), Some(WEB2));
        assert_eq!(web2.stdin.as_deref(), Some("aa\naal\n"));
        assert!(
            provision_plan(None)
                .iter()
                .all(|s| s.label != "web2" && !s.argv.iter().any(|a| a == WEB2))
        );
    }

    #[test]
    fn the_x86_64_environment_is_a_cross_environment() {
        let native = cargo_env(Arch::Aarch64, "/t", true);
        assert!(native.iter().all(|(k, _)| k != "CARGO_BUILD_TARGET"));
        assert!(
            native.iter().all(|(k, _)| k != "RUST_TEST_THREADS"),
            "aarch64 runs its tests in parallel"
        );
        assert!(native.contains(&("RUSTUP_TOOLCHAIN".into(), RUST.into())));
        let cross = cargo_env(Arch::X86_64, "/t", true);
        assert!(cross.contains(&(
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER".into(),
            "x86_64-linux-gnu-gcc".into()
        )));
        assert!(cross.contains(&(
            "CARGO_BUILD_TARGET".into(),
            "x86_64-unknown-linux-gnu".into()
        )));
        assert!(
            cross.contains(&("RUST_TEST_THREADS".into(), "1".into())),
            "Rosetta runs x86_64 tests one at a time"
        );
        let pinned_by_args = cargo_env(Arch::X86_64, "/t", false);
        assert!(
            pinned_by_args
                .iter()
                .all(|(k, _)| k != "CARGO_BUILD_TARGET")
        );
        assert_eq!(
            with_env(&[("A".into(), "1".into())], &["cargo".into()]),
            ["env", "A=1", "cargo"]
        );
    }

    #[test]
    fn displays_start_their_servers_and_point_clients_at_them() {
        let rt = "/run/user/501";
        let config = Config::from_created("/tmp/aterm-lane-config.ABCdef1234\n").unwrap();
        let x = Display::Xvfb.units(rt, &config);
        assert_eq!(x.len(), 1);
        assert_eq!(x[0].socket, "/tmp/.X11-unix/X91");
        assert_eq!(
            Display::Xvfb.client_env(rt),
            ("DISPLAY".into(), ":91".into())
        );
        let w = Display::Weston.units(rt, &config);
        assert_eq!(w[0].socket, "/run/user/501/aterm-lane-weston");
        assert_eq!(
            Display::Weston.client_env(rt).1,
            "/run/user/501/aterm-lane-weston"
        );
        let xw = Display::Xwayland.units(rt, &config);
        assert_eq!(xw.len(), 2, "weston first, then Xwayland inside it");
        assert_eq!(xw[0], w[0]);
        assert_eq!(xw[1].socket, "/tmp/.X11-unix/X92");
        assert!(xw[1].env.contains(&(
            "WAYLAND_DISPLAY".into(),
            "/run/user/501/aterm-lane-weston".into()
        )));
        assert_eq!(Display::Mutter.protocol(), "Wayland");
        let wx = Display::WestonX11.units(rt, &config);
        assert_eq!(wx.len(), 2, "Xvfb first, then weston inside it");
        assert_eq!(wx[0], x[0]);
        assert!(wx[1].argv.contains(&"--backend=x11".to_string()));
        assert!(wx[1].env.contains(&("DISPLAY".into(), ":91".into())));
        assert_eq!(
            Display::WestonX11.client_env(rt),
            (
                "WAYLAND_DISPLAY".into(),
                "/run/user/501/aterm-lane-weston-x11".into()
            )
        );
        assert_eq!(wx[1].socket, Display::WestonX11.client_env(rt).1);
        assert_eq!(Display::WestonX11.seat(), Some(Seat::Xtest));
        assert_eq!(Display::Mutter.seat(), Some(Seat::RemoteDesktop));
        assert_eq!(Display::Weston.seat(), None);
        assert!(!Display::PROOF.contains(&Display::WestonX11));
        for d in Display::ALL {
            assert!(Display::PROOF.contains(&d.proof_display()), "{d:?}");
        }
        assert_eq!(Display::WestonX11.proof_display(), Display::Weston);
        for d in Display::ALL {
            assert_eq!(Display::parse(d.name()), Some(d));
            for u in d.units(rt, &config) {
                assert!(u.name.starts_with(UNIT_PREFIX));
            }
        }
    }

    /// Every weston the lane starts reads a `weston.ini` its screen writes
    /// first, in the proof and in the baseline, so none starts with its
    /// shell's fade over the pixels a row holds; a screen with no weston
    /// writes none.
    #[test]
    fn every_weston_reads_the_ini_its_screen_writes() {
        let rt = "/run/user/501";
        let config = Config::from_created("/tmp/aterm-lane-config.ABCdef1234\n").unwrap();
        let ini_path = config.weston_ini();
        let config_arg = format!("--config={ini_path}");
        let screens = Screen::BASELINE.into_iter().chain(Screen::DECK);
        for screen in screens {
            let westons = |units: Vec<Unit>| {
                units
                    .into_iter()
                    .filter(|u| u.argv.first().is_some_and(|a| a == "weston"))
                    .count()
            };
            let reads = |units: Vec<Unit>| {
                units
                    .iter()
                    .filter(|u| u.argv.contains(&config_arg))
                    .count()
            };
            let ini = |files: Vec<(String, String)>| {
                files
                    .into_iter()
                    .filter(|(path, _)| path == &ini_path)
                    .map(|(_, text)| text)
                    .collect::<Vec<_>>()
            };
            let n = westons(screen.units(rt, &config));
            assert_eq!(n, usize::from(screen.display.runs_weston()), "{screen:?}");
            assert_eq!(reads(screen.units(rt, &config)), n, "{screen:?}");
            assert_eq!(reads(screen.baseline_units(rt, &config)), n, "{screen:?}");
            let written = vec![WESTON_INI_TEXT.to_string(); n];
            assert_eq!(ini(screen.files(&config)), written, "{screen:?}");
            assert_eq!(ini(screen.baseline_files(&config)), written, "{screen:?}");
        }
        let other = Config::from_created("/tmp/aterm-lane-config.9876ZYXwvu\n").unwrap();
        for screen in Screen::BASELINE.into_iter().chain(Screen::DECK) {
            for (run, foreign) in [(&config, &other), (&other, &config)] {
                for (path, _) in screen
                    .files(run)
                    .into_iter()
                    .chain(screen.baseline_files(run))
                {
                    assert!(run.contains(&path), "{screen:?}: {path}");
                    assert!(!foreign.contains(&path), "another run owns {path}");
                }
                if let Some(mutter) = screen.baseline_config(run) {
                    let keyfile = format!("{mutter}/glib-2.0/settings/keyfile");
                    let files = screen.baseline_files(run);
                    assert!(files.iter().any(|(path, _)| path == &keyfile));
                    for unit in screen
                        .baseline_units(rt, run)
                        .into_iter()
                        .chain(screen.gnome_shell(rt, run))
                    {
                        assert!(
                            unit.env
                                .contains(&("XDG_CONFIG_HOME".into(), mutter.clone()))
                        );
                    }
                }
                assert!(run.contains(&format!("{}/glib-2.0/settings/keyfile", run.ibus())));
            }
        }
        for unbound in [
            "/tmp/aterm-lane-weston-config/weston.ini".into(),
            format!("{}-sibling/weston.ini", config.root),
            format!("{}/../foreign", config.root),
            format!("{}//weston.ini", config.root),
            config.root.clone(),
        ] {
            assert!(!config.contains(&unbound), "{unbound}");
        }
        for invalid in [
            "",
            "/tmp/aterm-lane-config.123",
            "/tmp/aterm-lane-config.ABCdef1234/child\n",
            "/tmp/aterm-lane-config.ABCdef1234\nwarning\n",
            "/tmp/aterm-lane-config.ABCdef1234\n\n",
        ] {
            assert!(Config::from_created(invalid).is_none(), "{invalid:?}");
        }
    }

    /// The baseline's `us intl` goes on the X server it names as soon as that
    /// server listens: under weston's x11 backend before weston, which reads
    /// the keymap once as it starts.
    #[test]
    fn the_baseline_keymap_follows_the_x_server_it_sets() {
        let rt = "/run/user/501";
        let config = Config::from_created("/tmp/aterm-lane-config.ABCdef1234\n").unwrap();
        for (screen, server) in [
            (Screen::plain(Display::Xvfb), 0),
            (Screen::plain(Display::Xwayland), 1),
            (Screen::plain(Display::WestonX11), 0),
        ] {
            let (socket, step) = screen.baseline_keymap().expect("an X display");
            let units = screen.baseline_units(rt, &config);
            let at = units.iter().position(|u| u.socket == socket);
            assert_eq!(at, Some(server), "{}", screen.name());
            assert!(step.argv.contains(&"intl".to_string()));
        }
        assert!(Screen::plain(Display::Mutter).baseline_keymap().is_none());
        assert!(Screen::plain(Display::Weston).baseline_keymap().is_none());
    }

    #[test]
    fn a_unit_is_a_collected_user_unit_with_its_environment() {
        let unit = Unit {
            name: "aterm-lane-x".into(),
            env: vec![("K".into(), "v w".into())],
            argv: vec!["prog".into(), "--flag".into()],
            socket: "/s".into(),
        };
        assert_eq!(
            run_unit(&unit, Some("/home")),
            [
                "systemd-run",
                "--user",
                "--quiet",
                "--collect",
                "--unit=aterm-lane-x",
                "--working-directory=/home",
                "--setenv=K=v w",
                "--",
                "prog",
                "--flag"
            ]
        );
        assert_eq!(wait_socket("/s", 60)[..2], ["timeout", "60"]);
        assert_eq!(wait_socket("/s", 60).last().map(String::as_str), Some("/s"));
        let lines = wait_lines("/log", "it's \"joined\"", 2, 60);
        assert_eq!(lines[..2], ["timeout", "60"]);
        assert_eq!(lines[lines.len() - 3..], ["/log", "it's \"joined\"", "2"]);
        assert!(!lines[4].contains("joined"), "the text is an argument");
        let name = wait_name("org.freedesktop.portal.Desktop", 60);
        assert_eq!(name[..2], ["timeout", "60"]);
        assert_eq!(
            name.last().map(String::as_str),
            Some("org.freedesktop.portal.Desktop")
        );
    }

    /// The clipboard's second client: its owner is a lane unit, so the
    /// teardown after each display stops it however the row ended, and the
    /// text reaches xclip as an argument, never as shell source.
    #[test]
    fn the_clipboard_owner_is_a_lane_unit_on_the_display() {
        let owner = clipboard_owner(":91", Selection::Clipboard, "A'B");
        assert!(owner.name.starts_with(UNIT_PREFIX));
        assert_eq!(owner.env, [("DISPLAY".to_string(), ":91".to_string())]);
        assert_eq!(owner.argv[4], "A'B");
        assert_eq!(owner.argv.last().map(String::as_str), Some("clipboard"));
        assert!(!owner.argv[2].contains("A'B"));
        for argv in [
            read_clipboard(":91", Selection::Primary, 60),
            wait_clipboard(":91", Selection::Primary, "A'B", 60),
        ] {
            assert_eq!(argv[..4], ["env", "DISPLAY=:91", "timeout", "60"]);
        }
        assert_eq!(
            wait_clipboard(":91", Selection::Clipboard, "A'B", 60)[8],
            "A'B"
        );
    }

    #[test]
    fn only_an_enabled_registration_runs_x86_64() {
        let measured = "enabled\ninterpreter /mnt/lima-rosetta/rosetta\nflags: OCF\noffset 0\n";
        assert!(rosetta_registered(measured));
        assert!(!rosetta_registered(
            &measured.replace("enabled", "disabled")
        ));
        assert!(!rosetta_registered(
            "cat: /proc/sys/fs/binfmt_misc/rosetta: No such file"
        ));
    }

    #[test]
    fn the_rosetta_cache_is_emptied_with_its_daemon_stopped_and_stamped() {
        let steps = rosetta_cache_reset("abc-123\n");
        let labels: Vec<&str> = steps.iter().map(|s| s.label).collect();
        assert_eq!(
            labels,
            [
                "stop rosettad",
                "empty the AOT cache",
                "stamp the boot",
                "start rosettad"
            ]
        );
        assert!(steps[1].argv.contains(&ROSETTA_CACHE.to_string()));
        assert_eq!(steps[2].stdin.as_deref(), Some("abc-123\n"));
        assert_eq!(
            steps[2].argv.last().map(String::as_str),
            Some(ROSETTA_CACHE_BOOT)
        );
    }

    #[test]
    fn only_a_unix_address_is_an_a11y_bus() {
        assert_eq!(
            parse_a11y_address("s \"unix:path=/run/user/501/at-spi/bus_0,guid=ab\"\n"),
            Some("unix:path=/run/user/501/at-spi/bus_0,guid=ab")
        );
        assert_eq!(parse_a11y_address("s \"\""), None);
        assert_eq!(parse_a11y_address("Call failed: no such name"), None);
    }
}
