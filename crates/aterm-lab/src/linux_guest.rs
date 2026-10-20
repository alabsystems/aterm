// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the Linux lane's VM holds and how a command runs in it, as data: the
//! provisioning plan (apt sources, packages, the Rust toolchain), the
//! environment per architecture, and the display sessions. Each is an argv or
//! a text this module renders and [`crate::linux`] hands to `limactl shell`,
//! so all of it is tested here without a VM.
//!
//! The guest is Ubuntu 24.04 LTS on aarch64. x86_64 is a second dpkg
//! architecture (`amd64`, from archive.ubuntu.com) whose libraries the
//! x86_64 test binaries and the x86_64 aterm-gui load, and whose binaries run
//! under Rosetta's binfmt registration, which lima makes at boot.

use std::fmt::Write as _;

/// STOCK EXCEPTION: no Linux build of the Trust toolchain is published (the
/// atpkg `trust` releases carry the aarch64-apple-darwin asset alone, checked
/// 2026-10-16), and Trust ships only its host's std, so the guest compiles
/// with rustup's stock toolchain at this version, plus the x86_64 std.
pub const RUST: &str = "1.99.1";

/// A CPU architecture the lane tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    /// The guest's own: built and run natively.
    Aarch64,
    /// Cross-built with the x86_64 GNU toolchain and run under Rosetta: the
    /// cell the owner ships first.
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

    /// The `cargo` arguments that select this architecture's target: none for
    /// the guest's own, which `cargo` builds by default.
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

/// The guest's arm64 sources, written over the image's own (which name no
/// architecture, and so would also be asked for amd64 indexes that
/// ports.ubuntu.com does not carry).
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

/// The development libraries the X11, Wayland and xkb code links or loads
/// (the winit fork, softbuffer, x11-dl's `build.rs` probes, wgpu's GL and
/// Vulkan loaders), installed for both architectures.
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

/// Mesa's software drivers, installed for both architectures: llvmpipe (GL)
/// and lavapipe (Vulkan), the lane's GPU.
pub const MESA: &[&str] = &["libgl1-mesa-dri", "libegl-mesa0", "mesa-vulkan-drivers"];

/// The guest's own tools: the build, the display servers, the second X
/// client of the clipboard round trip, the baseline's input and probe tools
/// (xdotool's XTEST keys and pointer, xset and xrdb, xdpyinfo, wayland-info,
/// wl-clipboard's second Wayland client, ibus as the XIM server), the session
/// bus and the accessibility bus, the GPU probes, fonts (a stock Ubuntu
/// desktop's: Noto CJK and Noto Color Emoji are the CJK and colour faces
/// aterm-render's script and emoji tests need, and without them those answer
/// COULD NOT RUN), the x86_64 cross toolchain with its libc, and zsh and fish,
/// the shells aterm integrates beside bash, whose integration tests run each
/// one and cannot run without it.
pub const TOOLS: &[&str] = &[
    "build-essential",
    "pkg-config",
    "ca-certificates",
    "curl",
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
    "dbus",
    "dbus-x11",
    "dbus-user-session",
    "at-spi2-core",
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
];

/// x86_64 packages beyond the shared lists: the loader and libc the
/// translated binaries start on, and the C++ runtime Mesa links.
pub const AMD64_RUNTIME: &[&str] = &["libc6", "libgcc-s1", "libstdc++6"];

/// Every package the guest installs, `:amd64` names last.
#[must_use]
pub fn packages() -> Vec<String> {
    let native = TOOLS.iter().chain(DEV_LIBRARIES).chain(MESA);
    let foreign = DEV_LIBRARIES.iter().chain(MESA).chain(AMD64_RUNTIME);
    native
        .map(|p| (*p).to_string())
        .chain(foreign.map(|p| format!("{p}:amd64")))
        .collect()
}

/// Where the guest records the plan it was provisioned with.
pub const STAMP: &str = "/var/lib/aterm-lane/provisioned";

/// Webster's Second International (`web2`), the public-domain word list
/// aterm-lexicon's dictionary gates walk: their allowlists are curated against
/// it, so another list decides nothing and they answer COULD NOT RUN without it
/// (`crates/aterm-lexicon/tests/lexicon_suite.rs`). macOS ships it here and
/// Ubuntu packages no web2 (its `wamerican` is another list), so the guest gets
/// this Mac's copy at the same path.
pub const WEB2: &str = "/usr/share/dict/web2";

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

/// The provisioning plan, in order, with this Mac's [`WEB2`] when it reads
/// ([`provision_plan`]).
#[must_use]
pub fn provision_steps() -> Vec<Step> {
    provision_plan(std::fs::read_to_string(WEB2).ok())
}

/// The provisioning plan, in order, laying `web2` at [`WEB2`] when it is
/// given. Every step is idempotent, so an interrupted plan is simply run again.
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

/// The plan's fingerprint (FNV-1a over every step), which [`STAMP`] holds
/// once it ran: a changed plan provisions again.
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

/// The environment `cargo` gets for `arch`: the pinned stock toolchain, no
/// incremental cache, the guest-local target dir, and for x86_64 the cross
/// linker, C compiler and pkg-config search path (`CARGO_BUILD_TARGET`
/// names x86_64, a cross triple here, only when `default_target` asks).
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
    /// Wayland: weston's headless backend. Weston 13 offers no xdg-decoration,
    /// so aterm draws its own frame here, as under Mutter: since LW.5 the chrome
    /// band is the header (sctk-adwaita's before).
    Weston,
    /// Wayland: Mutter in headless mode, GNOME's compositor (client-side
    /// decorations).
    Mutter,
    /// X11 on Wayland: a rootful Xwayland inside headless weston.
    Xwayland,
}

/// A user unit the lane starts in the guest (`systemd-run --user`), and the
/// socket that says it is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub env: Vec<(String, String)>,
    pub argv: Vec<String>,
    pub socket: String,
}

/// Every unit the lane starts is named with this prefix, and a teardown
/// stops them all by it.
pub const UNIT_PREFIX: &str = "aterm-lane-";

/// The X display numbers the lane takes: high, so a person's own session
/// never holds them.
const XVFB_DISPLAY: &str = ":91";
const XWAYLAND_DISPLAY: &str = ":92";

/// The virtual screen every server gets.
const WIDTH: &str = "1280";
const HEIGHT: &str = "800";

impl Display {
    pub const ALL: [Display; 4] = [
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
        }
    }

    /// The protocol a client speaks to it.
    #[must_use]
    pub const fn protocol(self) -> &'static str {
        match self {
            Display::Xvfb | Display::Xwayland => "X11",
            Display::Weston | Display::Mutter => "Wayland",
        }
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Display> {
        Display::ALL.into_iter().find(|d| d.name() == word)
    }

    /// Whether a client with no input device can own the clipboard and read it
    /// back. X11's selections need no input. A Wayland selection is set with a
    /// serial from a seat's keyboard or pointer, and neither headless server has
    /// one: weston advertises no `wl_seat`, Mutter's reports no capabilities, and
    /// aterm's `copy` answers `ERR pbcopy failed` under both (measured
    /// 2026-10-16).
    #[must_use]
    pub const fn clipboard_scriptable(self) -> bool {
        matches!(self, Display::Xvfb | Display::Xwayland)
    }

    /// The servers it needs, in start order. `runtime` is the guest user's
    /// `XDG_RUNTIME_DIR`, where a Wayland socket is made.
    #[must_use]
    pub fn units(self, runtime: &str) -> Vec<Unit> {
        // GL (Mesa's llvmpipe through surfaceless EGL), not pixman. Weston 13's
        // pixman renderer wraps a client's shm buffer in a pixman image at attach
        // (`pixman_renderer_attach_shm`, `wl_shm_buffer_get_data`) and reads it at
        // the next repaint. A `wl_shm_pool.resize` in between can mremap the pool
        // elsewhere, and weston then reads the old mapping and dies of SIGSEGV
        // inside libpixman. sctk's frame pool grows that way whenever a resize
        // makes the window larger, so weston crashed under aterm at `resize 30
        // 100` in most x86_64 runs, with the registry rustix 0.38 as well as with
        // crates/aterm-rustix-compat, and aterm then panicked on the dead
        // connection. The GL renderer reads the pool when it uploads, and never
        // crashed (the counts: docs/measured/linux-lw2-2026-10-16.md).
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
            ]),
            socket: format!("{runtime}/{UNIT_PREFIX}weston"),
        };
        match self {
            Display::Xvfb => vec![Unit {
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
            }],
            Display::Weston => vec![weston()],
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

    /// What a client is told to reach it by: an X display, or the Wayland
    /// socket's absolute path (which libwayland takes as given, whatever the
    /// client's own `XDG_RUNTIME_DIR` is).
    #[must_use]
    pub fn client_env(self, runtime: &str) -> (String, String) {
        match self {
            Display::Xvfb => ("DISPLAY".into(), XVFB_DISPLAY.into()),
            Display::Xwayland => ("DISPLAY".into(), XWAYLAND_DISPLAY.into()),
            Display::Weston | Display::Mutter => (
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
    /// X11's: `Xft.dpi: 144` in the root window's RESOURCE_MANAGER (what a
    /// desktop's font-scaling setting writes), which the winit fork reads
    /// through aterm-x11's resource database (x11rb's before LW.7) as a scale
    /// of 1.5.
    XftDpi144,
    /// weston's headless output at `--scale=2`. Weston 13 offers no
    /// `wp_fractional_scale_manager_v1` (measured with wayland-info,
    /// 2026-10-16), so a whole number is the only scale it can give.
    Two,
    /// Mutter's virtual monitor at 1920x1200 with a logical scale of 1.5,
    /// sent to the client through `wp_fractional_scale_v1`. Mutter offers a
    /// fractional scale only with its `scale-monitor-framebuffer`
    /// experimental feature, read here from a GSettings keyfile, and takes the
    /// scale from a `monitors.xml`; both live in [`MUTTER_CONFIG`].
    OneAndHalf,
}

/// The scratch `XDG_CONFIG_HOME` of the fractional Mutter.
pub const MUTTER_CONFIG: &str = "/tmp/aterm-lane-mutter-config";

/// The scratch `XDG_CONFIG_HOME` of the baseline's ibus: its daemon, the XIM
/// server the daemon starts and the `ibus` command that picks the engine share
/// it (the daemon's bus address is written under it), and it holds
/// [`IBUS_KEYFILE`].
pub const IBUS_CONFIG: &str = "/tmp/aterm-lane-ibus-config";

/// The GSettings keyfile that keeps ibus off the X keymap. Unless
/// `use-system-keyboard-layout` is set, `ibus engine` runs `setxkbmap -layout`
/// with the engine's layout (`us` for `xkb:us::eng`), which drops the
/// baseline's `us intl` and with it the dead key's keycode (ibus 1.5.29's
/// `tools/main.vala`; measured: the X keymap read `us` after the XIM item).
pub const IBUS_KEYFILE: &str = "[desktop/ibus/general]\nuse-system-keyboard-layout=true\n";

/// The GSettings keyfile that turns on Mutter's fractional scaling.
pub const MUTTER_KEYFILE: &str =
    "[org/gnome/mutter]\nexperimental-features=['scale-monitor-framebuffer']\n";

/// The `monitors.xml` that puts Mutter's one virtual monitor (connector
/// `Meta-0`, as Mutter 46 names it) at 1920x1200, scale 1.5: a logical
/// 1280x800, the size of every other screen.
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

/// A display as the baseline runs it: a server, and the scale its output
/// carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub display: Display,
    pub scale: Scale,
}

impl Screen {
    /// Every screen the baseline records, in the order it takes them.
    pub const BASELINE: [Screen; 7] = [
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
    pub fn units(self, runtime: &str) -> Vec<Unit> {
        let mut units = self.display.units(runtime);
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
                    u.env.push(("XDG_CONFIG_HOME".into(), MUTTER_CONFIG.into()));
                }
            }
        }
        units
    }

    /// The files its servers read, written before they start: `(path, text)`.
    #[must_use]
    pub fn files(self) -> Vec<(String, String)> {
        match self.scale {
            Scale::OneAndHalf => vec![
                (
                    format!("{MUTTER_CONFIG}/glib-2.0/settings/keyfile"),
                    MUTTER_KEYFILE.into(),
                ),
                (
                    format!("{MUTTER_CONFIG}/monitors.xml"),
                    MUTTER_MONITORS.into(),
                ),
            ],
            _ => Vec::new(),
        }
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

    /// The baseline's X keymap, `us intl`, set before its client starts (an
    /// X11 screen's only; the proof's rows and `exec` keep the server's own
    /// `us`, where an apostrophe typed through a real key stays one). It has
    /// the dead keys the compose and XIM checks type: xdotool types a keysym
    /// the keymap lacks by remapping a spare keycode for the one key, and a
    /// client that reads the key before the MappingNotify loses it (measured
    /// under plain `us`: `dead_acute e` sometimes typed a bare `e`, with and
    /// without XIM, on both architectures).
    #[must_use]
    pub fn baseline_keymap(self) -> Option<Step> {
        (self.display.protocol() == "X11").then(|| Step {
            label: "keymap us intl",
            argv: strings(&[
                "env",
                &format!("DISPLAY={}", self.display.client_env("").1),
                "setxkbmap",
                "-layout",
                "us",
                "-variant",
                "intl",
            ]),
            stdin: None,
        })
    }
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|s| (*s).to_string()).collect()
}

fn x_socket(display: &str) -> String {
    format!("/tmp/.X11-unix/X{}", display.trim_start_matches(':'))
}

/// `systemd-run --user … -- ARGV`: `unit` started as a transient user unit,
/// collected when it ends, so the guest keeps nothing of a run.
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

/// Wait, in the guest, until `socket` exists: `timeout` ends the wait (exit
/// 124) after `secs`, a hang detector. The loop is a fixed literal; the path
/// is its one argument.
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

/// Wait, in the guest, until the file `path` exists: a hang detector, as
/// [`wait_socket`].
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

/// Send `signal` to the main process of the user unit `name`: SIGKILL ends a
/// server as a crash does, with no goodbye to its clients, and SIGSTOP freezes
/// it. Only the main one: weston's unit refused a SIGKILL to its auxiliary
/// processes (`Invalid argument`, measured 2026-10-17), and the unit's stop
/// takes the rest.
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

/// Wait, in the guest, until a socket of the user unit `name`'s main process
/// holds bytes it has not read (`ss`'s Recv-Q): what a client sent a frozen
/// server. A hang detector, as [`wait_socket`].
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

/// Wait, in the guest, until the user unit `name` is no longer active: a
/// hang detector, as [`wait_socket`].
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
    /// PRIMARY: what a selection made with the mouse offers, and a middle
    /// click pastes.
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

/// What a second X client (xclip) reads as `sel` on the X `display`, on a
/// line of its own: the X server asks the owner by `ConvertSelection` and
/// carries its answer. `timeout` is a hang detector on an owner that never
/// answers.
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

/// xclip owning `sel` on the X `display` with `text`, as a lane unit:
/// `-quiet` keeps it in the foreground, serving `text` until another client
/// takes the selection or [`stop_units`] stops it.
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

/// Wait, in the guest, until a reader of the X `display`'s `sel` gets
/// `text`: the owner [`clipboard_owner`] started has taken the selection. A
/// hang detector, as [`wait_socket`].
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

/// Where the guest kernel shows Rosetta's binfmt registration, which lima
/// makes at boot (`vmOpts.vz.rosetta.binfmt`): without it no x86_64 binary
/// runs.
pub const ROSETTA_BINFMT: &str = "/proc/sys/fs/binfmt_misc/rosetta";

/// Whether [`ROSETTA_BINFMT`]'s text is an enabled registration of an
/// interpreter.
#[must_use]
pub fn rosetta_registered(text: &str) -> bool {
    let mut lines = text.lines();
    lines.next() == Some("enabled") && lines.any(|l| l.starts_with("interpreter /"))
}

/// Rosetta's AOT cache in the guest, which lima's `rosettad` keeps. A cache
/// written in an earlier boot is unusable: measured on 2026-10-16, the x86_64
/// aterm-gui then aborts in Rosetta (`assertion failed
/// [aot_hdr->segment_count <= kMaxSegments]: AOT header specified too many
/// segments`, ImageInfo.cpp:71) as soon as it loads Mesa, while the same
/// binaries run with a cache written in the current boot. So the lane empties
/// it once per boot, and stamps it with that boot.
pub const ROSETTA_CACHE: &str = "/var/cache/rosettad";

/// The stamp: the boot ([`BOOT_ID`]) the cache was emptied in.
pub const ROSETTA_CACHE_BOOT: &str = "/var/cache/rosettad/aterm-lane-boot-id";

/// The guest kernel's id of this boot.
pub const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

/// Empty Rosetta's AOT cache and stamp it with `boot_id`; `rosettad` is
/// stopped around the removal so it writes nothing half-way.
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

/// Ask the session bus for the accessibility bus (`org.a11y.Bus.GetAddress`),
/// which D-Bus activates at-spi2-core's bus launcher when it is not up yet.
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
        changed[4].argv.push("extra-package".into());
        assert_ne!(fingerprint(&plan), fingerprint(&changed));
        let mut sources = plan.clone();
        sources[0].stdin = Some(SOURCES_AMD64.into());
        assert_ne!(fingerprint(&plan), fingerprint(&sources));
        // A changed word list provisions again too.
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
        let x = Display::Xvfb.units(rt);
        assert_eq!(x.len(), 1);
        assert_eq!(x[0].socket, "/tmp/.X11-unix/X91");
        assert_eq!(
            Display::Xvfb.client_env(rt),
            ("DISPLAY".into(), ":91".into())
        );
        let w = Display::Weston.units(rt);
        assert_eq!(w[0].socket, "/run/user/501/aterm-lane-weston");
        assert_eq!(
            Display::Weston.client_env(rt).1,
            "/run/user/501/aterm-lane-weston"
        );
        let xw = Display::Xwayland.units(rt);
        assert_eq!(xw.len(), 2, "weston first, then Xwayland inside it");
        assert_eq!(xw[0], w[0]);
        assert_eq!(xw[1].socket, "/tmp/.X11-unix/X92");
        assert!(xw[1].env.contains(&(
            "WAYLAND_DISPLAY".into(),
            "/run/user/501/aterm-lane-weston".into()
        )));
        assert_eq!(Display::Mutter.protocol(), "Wayland");
        for d in Display::ALL {
            assert_eq!(Display::parse(d.name()), Some(d));
            for u in d.units(rt) {
                assert!(u.name.starts_with(UNIT_PREFIX));
            }
        }
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
