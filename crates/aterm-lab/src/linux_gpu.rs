// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux lane's GPU instance (`lab linux --gpu <verb>`, L-REALGPU): a second lima VM,
//! `aterm-linux-gpu`, on krunkit (libkrun's virtio-gpu with virglrenderer's Venus), so the
//! guest's Vulkan and GL reach this Mac's GPU instead of lavapipe and llvmpipe.
//!
//! What the guest's GPU needs, as measured on 2026-10-24 (docs/measured/linux-realgpu-2026-10-24.md):
//! - krunkit 1.3.2 (Homebrew's `slp/krun` tap) offers `VIRTIO_GPU_F_BLOB_ALIGNMENT` (the host's
//!   16 KiB page). Venus maps its ring only with a kernel that reads it (Linux 7.2,
//!   `VIRTGPU_PARAM_BLOB_ALIGNMENT`) and a Mesa that honours it (26.3, MR !44783): noble's 6.8
//!   kernel and Mesa 25.2.8 fail `vkCreateDevice` with `VK_ERROR_OUT_OF_HOST_MEMORY`. The guest
//!   gets the mainline 7.2.6 kernel ([`KERNEL`]) and a Venus-only Mesa 26.3 build ([`VENUS`]).
//! - zink needs `nullDescriptor`, which MoltenVK 1.4.2 (krunkit's own) lacks and KosmicKrisp (the
//!   LunarG SDK's Vulkan 1.4 driver on Metal) has: krunkit loads KosmicKrisp through the SDK's
//!   loader ([`SDK`], placed by `linux gpu-host`), named `libMoltenVK.dylib` in a directory on
//!   `DYLD_LIBRARY_PATH` (krunkit has no library validation).
//! - Mesa 26.3's zink requires `VK_KHR_maintenance5`, which virglrenderer 0.10.4e does not pass
//!   through; noble's zink (Mesa 25.2.8) runs on the guest's Venus with GL 2.1 and GLES 2.0
//!   (no transform feedback), and `MESA_GLES_VERSION_OVERRIDE=3.0` gives the GLES 3.0 context
//!   the GL arm asks for ([`client_env`]).
//!
//! krunkit's guests are aarch64 with no Rosetta: the instance runs no x86_64 row ([`lane_arches`]).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use crate::linux_guest::{Arch, Step};
use crate::{Fail, LAB, Res};

/// The GPU instance's lima name.
pub const VM: &str = "aterm-linux-gpu";

/// Whether `--gpu` was named, for the rest of this process.
static ON: OnceLock<bool> = OnceLock::new();

/// `--gpu`, before the verb: every verb of this process runs on the GPU instance.
pub fn choose() {
    let _ = ON.set(true);
}

/// Whether this run is the GPU instance's.
#[must_use]
pub fn on() -> bool {
    ON.get().copied().unwrap_or(false)
}

/// The lima instance this run drives: [`VM`] under `--gpu`, else the lane's.
#[must_use]
pub fn vm() -> &'static str {
    if on() { VM } else { crate::linux::VM }
}

/// `limactl create`'s arguments for the GPU instance: the lane's size and template on krunkit,
/// whose lima driver shares a directory only over virtio-fs, and no Rosetta (krunkit has none).
#[must_use]
pub fn create_args() -> Vec<String> {
    use crate::linux::{CPUS, DISK_GIB, MEMORY_GIB, TEMPLATE};
    [
        "create".to_string(),
        format!("--name={VM}"),
        "--tty=false".into(),
        "--vm-type=krunkit".into(),
        "--mount-type=virtiofs".into(),
        "--arch=aarch64".into(),
        format!("--cpus={CPUS}"),
        format!("--memory={MEMORY_GIB}"),
        format!("--disk={DISK_GIB}"),
        "--containerd=none".into(),
        "--mount-none".into(),
        TEMPLATE.into(),
    ]
    .into()
}

/// The arches a GPU-instance verb runs: the named ones, or every one, on the lane's VM; under
/// `--gpu` aarch64 alone, and a named x86_64 refused with the reason.
///
/// # Errors
/// x86_64 named under `--gpu`.
pub fn lane_arches(named: Vec<Arch>, gpu: bool, verb: &str) -> Result<Vec<Arch>, Fail> {
    if !gpu {
        return Ok(if named.is_empty() {
            Arch::ALL.to_vec()
        } else {
            named
        });
    }
    if named.contains(&Arch::X86_64) {
        return Err(Fail::usage(format!(
            "linux --gpu {verb}: --arch x86_64: krunkit's guests are aarch64 with no Rosetta, \
             so no x86_64 build reaches the GPU instance's GPU (x86_64 runs on the lane's VM, \
             without --gpu)"
        )));
    }
    Ok(vec![Arch::Aarch64])
}

/// [`lane_arches`] for this run.
///
/// # Errors
/// As [`lane_arches`].
pub fn arches(named: Vec<Arch>, verb: &str) -> Result<Vec<Arch>, Fail> {
    lane_arches(named, on(), verb)
}

/// Where krunkit is looked for: Homebrew's prefixes, then the search path.
pub const KRUNKIT_AT: [&str; 2] = ["/opt/homebrew/bin/krunkit", "/usr/local/bin/krunkit"];

/// What the GPU instance says when it finds no krunkit: where it looked, and that aterm provides
/// none, with no install command (DESIGN-init-system §6.5, as rows 26 and 32 apply it to the lab).
fn no_krunkit() -> String {
    format!(
        "no krunkit at {} or on PATH: the GPU instance runs on your own krunkit (measured: krunkit \
         1.3.2, libkrun 1.19.7, virglrenderer 0.10.4e), and aterm provides none (no check set runs \
         this lane)",
        KRUNKIT_AT.join(" or ")
    )
}

/// The LunarG Vulkan SDK whose loader and KosmicKrisp driver krunkit's virglrenderer loads.
pub struct Sdk {
    /// The SDK's version.
    pub version: &'static str,
    /// The macOS archive.
    pub url: &'static str,
    /// Its SHA-256 (measured 2026-10-24; LunarG publishes none beside it).
    pub sha256: &'static str,
}

/// The SDK the instance is measured on (a release at least two weeks old).
pub const SDK: Sdk = Sdk {
    version: "1.4.363.0",
    url: "https://sdk.lunarg.com/sdk/download/1.4.363.0/mac/vulkansdk-macos-1.4.363.0.zip",
    sha256: "163431e0203c8215286a17cf393643ce6b951f77d1b4f74ab30821a1f0beb09b",
};

/// The files placed from the SDK's install, each `(source under the install root, placed path)`:
/// the loader, KosmicKrisp and its ICD manifest (whose `library_path`, `../../../lib/…`, the
/// placed layout keeps), and the SDK's licence.
pub const PLACED: [(&str, &str); 4] = [
    ("macOS/lib/libvulkan.1.dylib", "lib/libvulkan.1.dylib"),
    (
        "macOS/lib/libvulkan_kosmickrisp.dylib",
        "lib/libvulkan_kosmickrisp.dylib",
    ),
    (
        "macOS/share/vulkan/icd.d/libkosmickrisp_icd.json",
        "share/vulkan/icd.d/libkosmickrisp_icd.json",
    ),
    ("Licenses/LICENSE.txt", "LICENSE.txt"),
];

/// The directory on `DYLD_LIBRARY_PATH` that names the loader `libMoltenVK.dylib`, the leaf
/// virglrenderer links.
pub const SHIM: &str = "shim";

/// KosmicKrisp's place in the tooling cache `tooling`.
#[must_use]
pub fn kk_dir(tooling: &Path) -> PathBuf {
    tooling.join(format!("kosmickrisp-{}", SDK.version))
}

/// The environment `limactl` (and so lima's krunkit driver and krunkit) runs with: the shim
/// first on `DYLD_LIBRARY_PATH`, the loader told to load KosmicKrisp alone, and krunkit's
/// directory first on `PATH` (`path`, this process's).
#[must_use]
pub fn host_env(kk: &Path, krunkit: &Path, path: Option<&str>) -> Vec<(String, String)> {
    let bin = krunkit
        .parent()
        .map_or_else(String::new, |d| d.to_string_lossy().into_owned());
    let path = match path.filter(|p| !p.is_empty()) {
        Some(p) => format!("{bin}:{p}"),
        None => format!("{bin}:/usr/bin:/bin:/usr/sbin:/sbin"),
    };
    vec![
        (
            "DYLD_LIBRARY_PATH".into(),
            kk.join(SHIM).to_string_lossy().into_owned(),
        ),
        (
            "VK_DRIVER_FILES".into(),
            kk.join(PLACED[2].1).to_string_lossy().into_owned(),
        ),
        ("PATH".into(), path),
    ]
}

/// krunkit on this Mac.
///
/// # Errors
/// NOT RUN: none, naming where it looked, with no install command.
pub fn krunkit() -> Result<PathBuf, Fail> {
    let on_path = std::env::var("PATH").unwrap_or_default();
    KRUNKIT_AT
        .iter()
        .map(PathBuf::from)
        .chain(
            on_path
                .split(':')
                .filter(|d| d.starts_with('/'))
                .map(|d| Path::new(d).join("krunkit")),
        )
        .find(|p| crate::sys::executable(&p.to_string_lossy()))
        .ok_or_else(|| crate::linux::not_run(no_krunkit()))
}

/// The GPU instance's `limactl` environment ([`host_env`]), once krunkit and KosmicKrisp are here.
///
/// # Errors
/// NOT RUN: no Apple silicon, no krunkit, or KosmicKrisp not placed (`linux gpu-host`).
pub fn lima_env() -> Result<Vec<(String, String)>, Fail> {
    if !cfg!(target_arch = "aarch64") {
        return Err(crate::linux::not_run(
            "the GPU instance needs Apple silicon (krunkit's virtio-gpu)",
        ));
    }
    let krunkit = krunkit()?;
    let tooling = aterm_uds::host::tooling_dir()
        .ok_or_else(|| crate::linux::not_run("no tooling cache (HOME is unset)"))?;
    let kk = kk_dir(&tooling);
    if let Some((_, missing)) = PLACED
        .iter()
        .chain(&[("", "shim/libMoltenVK.dylib")])
        .find(|(_, placed)| !kk.join(placed).exists())
    {
        return Err(crate::linux::not_run(format!(
            "KosmicKrisp is not placed ({} has no {missing}): `{LAB} linux gpu-host` places it",
            kk.display()
        )));
    }
    Ok(host_env(
        &kk,
        &krunkit,
        std::env::var("PATH").ok().as_deref(),
    ))
}

/// `linux gpu-host`: place the SDK's loader and KosmicKrisp under the tooling cache: the archive
/// fetched and held to [`SDK`]'s digest, its installer run headless into a scratch root
/// (`copy_only=1`: nothing outside that root), [`PLACED`] copied out with the shim beside them,
/// the scratch removed.
///
/// # Errors
/// A usage error, a download or digest that failed, or an installer that placed nothing.
pub fn cmd_gpu_host(args: &[String]) -> Res {
    if let Some(a) = args.first() {
        return Err(Fail::usage(format!(
            "linux gpu-host: unknown argument '{a}' ({LAB} linux help)"
        )));
    }
    let tooling = aterm_uds::host::tooling_dir()
        .ok_or_else(|| Fail::die("linux gpu-host: no tooling cache (HOME is unset)"))?;
    let kk = kk_dir(&tooling);
    let placed = |kk: &Path| PLACED.iter().all(|(_, p)| kk.join(p).exists());
    if placed(&kk) && kk.join(SHIM).join("libMoltenVK.dylib").exists() {
        println!(
            "linux gpu-host: KosmicKrisp {} is placed at {}",
            SDK.version,
            kk.display()
        );
        return Ok(0);
    }
    let work = tooling.join(format!("kosmickrisp-{}.work", SDK.version));
    let _ = std::fs::remove_dir_all(&work);
    let result = place(&work, &kk);
    let _ = std::fs::remove_dir_all(&work);
    result.map_err(|e| Fail::die(format!("linux gpu-host: {e}")))?;
    println!(
        "linux gpu-host: KosmicKrisp {} placed at {}",
        SDK.version,
        kk.display()
    );
    Ok(0)
}

fn place(work: &Path, kk: &Path) -> Result<(), String> {
    let zip = work.join(format!("vulkansdk-macos-{}.zip", SDK.version));
    println!("linux gpu-host: fetching {} …", SDK.url);
    crate::windows::curl_file(SDK.url, &zip, false)?;
    let have = crate::windows::sha256_file(&zip)?;
    if have != SDK.sha256 {
        return Err(format!(
            "{} has sha256 {have}, not the pinned {}",
            SDK.url, SDK.sha256
        ));
    }
    let x = work.join("x");
    let out = crate::windows::run(
        Path::new("/usr/bin/ditto"),
        &[
            "-x".into(),
            "-k".into(),
            zip.to_string_lossy().into_owned(),
            x.to_string_lossy().into_owned(),
        ],
        None,
    );
    if !out.ok() {
        return Err(format!("ditto: {}", out.tail(3)));
    }
    let app = format!("vulkansdk-macOS-{}", SDK.version);
    let installer = x
        .join(format!("{app}.app"))
        .join("Contents/MacOS")
        .join(&app);
    let root = work.join("root");
    println!(
        "linux gpu-host: installing the SDK into {} (headless) …",
        root.display()
    );
    let out = crate::windows::run(
        &installer,
        &[
            "--root".into(),
            root.to_string_lossy().into_owned(),
            "--accept-licenses".into(),
            "--default-answer".into(),
            "--confirm-command".into(),
            "install".into(),
            "copy_only=1".into(),
        ],
        Some(work),
    );
    if !out.ok() {
        return Err(format!("the SDK installer: {}", out.tail(5)));
    }
    let _ = std::fs::remove_dir_all(kk);
    place_files(&root, kk, &PLACED)?;
    let shim = kk.join(SHIM);
    std::fs::create_dir_all(&shim).map_err(|e| format!("{}: {e}", shim.display()))?;
    std::os::unix::fs::symlink("../lib/libvulkan.1.dylib", shim.join("libMoltenVK.dylib"))
        .map_err(|e| format!("{}: {e}", shim.display()))
}

/// Copies each `(source under root, placed path)` of `files` from the install `root` into `kk`.
///
/// The SDK ships `libvulkan.1.dylib` as a link to its versioned dylib, so each source is
/// resolved first and must land inside `root`; the regular file it names is copied under the
/// placed name (the no-follow open refuses anything that is not a regular file by then).
fn place_files(root: &Path, kk: &Path, files: &[(&str, &str)]) -> Result<(), String> {
    let root = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    for (from, to) in files {
        let (src, dst) = (root.join(from), kk.join(to));
        let resolved =
            std::fs::canonicalize(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        if !resolved.starts_with(&root) {
            return Err(format!(
                "{} resolves to {}, outside the install root {}",
                src.display(),
                resolved.display(),
                root.display()
            ));
        }
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        crate::util::copy_regular_file(&resolved, &dst)
            .map_err(|e| format!("{}: {e}", resolved.display()))?;
    }
    Ok(())
}

/// The mainline kernel the guest boots: the first with `VIRTGPU_PARAM_BLOB_ALIGNMENT`, which
/// Venus needs on a 16 KiB-page host. Ubuntu's mainline build, arm64's 4 KiB flavour (Apple
/// silicon has no 64 KiB translation granule).
pub struct Kernel {
    /// `uname -r` once it boots.
    pub release: &'static str,
    /// The directory the packages are fetched from.
    pub base: &'static str,
    /// Each package, with its SHA-256 (measured 2026-10-24; the directory's CHECKSUMS agree).
    pub debs: [(&'static str, &'static str); 2],
}

/// The guest's kernel.
pub const KERNEL: Kernel = Kernel {
    release: "7.2.6-070206-generic",
    base: "https://kernel.ubuntu.com/mainline/v7.2.6/arm64",
    debs: [
        (
            "linux-modules-7.2.6-070206-generic_7.2.6-070206.202609141300_arm64.deb",
            "15598e55ef06a03dd24b4550f5375e2ec021675f6cbb50af414cd3af41866486",
        ),
        (
            "linux-image-unsigned-7.2.6-070206-generic_7.2.6-070206.202609141300_arm64.deb",
            "02aae8e32fe4d44d47d26b41b9800b9129f0e986840f5a72bdd04b7c78ef5ce8",
        ),
    ],
};

/// The kernel step: the packages held to their digests and installed, once. flash-kernel goes
/// first (it refuses a mainline kernel no DTB table names, which leaves it half-configured, and a
/// UEFI guest boots through grub without it). The packages are unpacked and configured with their
/// trigger held, which is then cut to the one hook directory noble has ([`TRIGGER_FIX`]) and run.
#[must_use]
pub fn kernel_script(k: &Kernel) -> String {
    let fetch: String = k
        .debs
        .iter()
        .enumerate()
        .map(|(i, (deb, _))| {
            format!(
                "curl -fsSL --retry 2 -o \"$d/{i}.deb\" '{}/{deb}'\n",
                k.base
            )
        })
        .collect();
    let sums: String = k
        .debs
        .iter()
        .enumerate()
        .map(|(i, (_, sha))| format!("{sha}  $d/{i}.deb\n"))
        .collect();
    let release = k.release;
    format!(
        r#"installed() {{ [ "$(dpkg-query -W -f='${{Status}}' "$1" 2>/dev/null)" = "install ok installed" ]; }}
if installed linux-modules-{release} && installed linux-image-unsigned-{release}; then exit 0; fi
d=$(mktemp -d)
trap 'rm -rf "$d"' EXIT
{fetch}sha256sum -c - <<SUMS
{sums}SUMS
export DEBIAN_FRONTEND=noninteractive
apt-get purge -y -q flash-kernel
apt-get install -y -q --no-install-recommends kmod linux-base wireless-regdb
dpkg --unpack "$d/0.deb" "$d/1.deb"
dpkg --configure --no-triggers linux-modules-{release} linux-image-unsigned-{release}
{TRIGGER_FIX}
dpkg --triggers-only -a
installed linux-image-unsigned-{release}
"#
    )
}

/// The mainline packages' trigger runs `run-parts` over `/etc/kernel/postinst.d` and
/// `/usr/share/kernel/postinst.d`, and noble's `run-parts` takes one directory (`missing
/// operand`, which leaves the kernel half-configured and every later `dpkg --configure -a`
/// failing). noble has no `/usr/share/kernel`, so the second goes from each held trigger.
pub const TRIGGER_FIX: &str = "for t in /usr/lib/linux/triggers/*; do [ -f \"$t\" ] && sed -i 's# /usr/share/kernel/postinst.d$##' \"$t\"; done; true";

/// A step that runs `script` as root, read whole into a file first: a script fed to `sh -s`
/// is read as it runs, so a command that reads its stdin (apt's, dpkg's) can eat the rest of it.
#[must_use]
pub fn script_step(label: &'static str, script: &str) -> Step {
    Step {
        label,
        argv: [
            "sudo",
            "sh",
            "-c",
            "f=$(mktemp) && cat >\"$f\" && sh -eu \"$f\" </dev/null; rc=$?; rm -f \"$f\"; exit $rc",
            "sh",
        ]
        .map(String::from)
        .to_vec(),
        stdin: Some(script.to_string()),
    }
}

/// Mesa's Venus driver for the guest: a release with MR !44783 ("venus: honor the virtio-gpu
/// blob alignment"), built with Venus alone (no GL: the guest's zink is noble's).
pub struct Venus {
    /// The Mesa release.
    pub mesa: &'static str,
    /// Its archive's SHA-256 (measured 2026-10-24; Mesa publishes a signature, no digest).
    pub mesa_sha256: &'static str,
    /// The meson Mesa 26.3 needs (noble's is 1.3; it asks for 1.4).
    pub meson: &'static str,
    /// Its archive's SHA-256 (measured 2026-10-24).
    pub meson_sha256: &'static str,
    /// The venus-protocol subproject's tag and the commit it must name.
    pub protocol: (&'static str, &'static str),
    /// Where it installs.
    pub prefix: &'static str,
}

/// The guest's Venus.
pub const VENUS: Venus = Venus {
    mesa: "26.3.0-rc2",
    mesa_sha256: "6b9154bc46ec73901555dbcb61600eb69607dc1da718a87e7a1c09a755bb1b6d",
    meson: "1.12.1",
    meson_sha256: "ab0a6ca09f8ef70c564c8241fb5a23957886a0b53fb58412b5e07eaf07dba743",
    protocol: ("v1.1.3", "ca19b6358d7cc491bc3e4de76f04c6700876a8fa"),
    prefix: "/opt/aterm-venus",
};

/// The Venus ICD manifest the guest's Vulkan loader is pointed at.
#[must_use]
pub fn icd(v: &Venus) -> String {
    format!("{}/share/vulkan/icd.d/virtio_icd.aarch64.json", v.prefix)
}

/// The Venus step: Mesa and meson fetched and held to their digests, venus-protocol cloned at its
/// tag and held to its commit, Mesa configured with downloads off (so no other subproject is
/// fetched) and Venus built and installed under the prefix, once per release.
#[must_use]
pub fn venus_script(v: &Venus) -> String {
    let (mesa, meson, prefix) = (v.mesa, v.meson, v.prefix);
    let (tag, commit) = v.protocol;
    format!(
        r#"[ -f {prefix}/.mesa-{mesa} ] && exit 0
d=$(mktemp -d)
trap 'rm -rf "$d"' EXIT
cd "$d"
curl -fsSL --retry 2 -o mesa.tar.xz 'https://archive.mesa3d.org/mesa-{mesa}.tar.xz'
curl -fsSL --retry 2 -o meson.tar.gz 'https://github.com/mesonbuild/meson/releases/download/{meson}/meson-{meson}.tar.gz'
sha256sum -c - <<SUMS
{mesa_sha}  mesa.tar.xz
{meson_sha}  meson.tar.gz
SUMS
tar xf mesa.tar.xz
tar xf meson.tar.gz
git clone -q --depth 1 --branch {tag} https://gitlab.freedesktop.org/virgl/venus-protocol.git mesa-{mesa}/subprojects/venus-protocol-{tag_ver}
test "$(git -C mesa-{mesa}/subprojects/venus-protocol-{tag_ver} rev-parse HEAD)" = {commit}
cd mesa-{mesa}
python3 ../meson-{meson}/meson.py setup build --wrap-mode=nodownload --prefix={prefix} --libdir=lib --buildtype=release -Dgallium-drivers= -Dvulkan-drivers=virtio -Dplatforms=x11,wayland -Dopengl=false -Dglx=disabled -Degl=disabled -Dgbm=disabled -Dgles1=disabled -Dgles2=disabled -Dglvnd=disabled -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled -Dvideo-codecs= -Dvulkan-layers= -Dtools= -Dandroid-libbacktrace=disabled -Dgallium-va=disabled -Dmicrosoft-clc=disabled -Dintel-rt=disabled
ninja -C build
rm -rf {prefix}
ninja -C build install
test -f {icd}
touch {prefix}/.mesa-{mesa}
"#,
        mesa_sha = v.mesa_sha256,
        meson_sha = v.meson_sha256,
        tag_ver = tag.trim_start_matches('v'),
        icd = icd(v),
    )
}

/// What the Venus build needs beyond the lane's packages (noble-updates carries the
/// wayland-protocols 1.45 and libdrm 2.4.125 Mesa 26.3 asks for).
pub const BUILD_PACKAGES: &[&str] = &[
    "git",
    "xz-utils",
    "ninja-build",
    "python3-mako",
    "python3-yaml",
    "python3-packaging",
    "bison",
    "flex",
    "glslang-tools",
    "spirv-tools",
    "libdrm-dev",
    "libwayland-dev",
    "wayland-protocols",
    "libx11-xcb-dev",
    "libxcb-dri3-dev",
    "libxcb-present-dev",
    "libxcb-randr0-dev",
    "libxcb-shm0-dev",
    "libxcb-sync-dev",
    "libxcb-xfixes0-dev",
    "libxshmfence-dev",
    "libxrandr-dev",
    "zlib1g-dev",
    "libzstd-dev",
    "libexpat1-dev",
];

/// The GPU instance's step before the lane's own plan, whose `dpkg --configure -a` would run a
/// kernel trigger an interrupted kernel step left held: [`TRIGGER_FIX`] on any there.
#[must_use]
pub fn gpu_first_steps() -> Vec<Step> {
    vec![script_step("mainline kernel trigger", TRIGGER_FIX)]
}

/// The GPU instance's steps after the lane's own (aarch64) plan: the Venus build's packages, the
/// kernel, and Venus.
#[must_use]
pub fn gpu_steps() -> Vec<Step> {
    let install: Vec<String> = [
        "sudo",
        "env",
        "DEBIAN_FRONTEND=noninteractive",
        "apt-get",
        "install",
        "-y",
        "-q",
        "--no-install-recommends",
    ]
    .iter()
    .chain(BUILD_PACKAGES)
    .map(|s| (*s).to_string())
    .collect();
    vec![
        Step {
            label: "venus build packages",
            argv: install,
            stdin: None,
        },
        script_step("mainline kernel", &kernel_script(&KERNEL)),
        script_step("venus", &venus_script(&VENUS)),
    ]
}

/// The environment a GPU-instance client gets (cargo and what it runs, aterm-gui): the loader
/// pointed at Venus alone (so neither wgpu nor zink can fall back to lavapipe), Mesa's GL loader
/// at zink, zink's GLES held at 3.0, kopper's X11 presents without DRI3 (Xvfb's has no
/// multibuffer), and Gallium's software screen at zink: EGL's Wayland platform cannot make its
/// DRI2 screen on Venus (no `VK_KHR_external_memory_fd`) and falls back to that software screen,
/// which is llvmpipe unless `GALLIUM_DRIVER` names zink (measured 2026-10-24 with `eglinfo -B -p
/// wayland` under weston: llvmpipe without it, zink over Venus with it). Empty off the GPU
/// instance. The display servers keep the lane's own (llvmpipe).
#[must_use]
pub fn client_env() -> Vec<(String, String)> {
    if on() { gpu_env() } else { Vec::new() }
}

/// [`client_env`]'s variables.
#[must_use]
pub fn gpu_env() -> Vec<(String, String)> {
    vec![
        ("VK_DRIVER_FILES".into(), icd(&VENUS)),
        ("MESA_LOADER_DRIVER_OVERRIDE".into(), "zink".into()),
        ("MESA_GLES_VERSION_OVERRIDE".into(), "3.0".into()),
        ("LIBGL_KOPPER_DRI2".into(), "1".into()),
        ("GALLIUM_DRIVER".into(), "zink".into()),
    ]
}

/// The Vulkan device `vulkaninfo --summary` names first (`deviceName = …`).
#[must_use]
pub fn vulkan_device(summary: &str) -> Option<&str> {
    summary.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "deviceName").then(|| v.trim())
    })
}

/// The GL renderer `glxinfo -B` names.
#[must_use]
pub fn gl_renderer(glxinfo: &str) -> Option<&str> {
    glxinfo.lines().find_map(|l| {
        l.trim()
            .strip_prefix("OpenGL renderer string:")
            .map(str::trim)
    })
}

/// Whether the guest's GPU is this Mac's: Vulkan on Venus over Apple's GPU, and GL on zink over
/// that Venus (never lavapipe, llvmpipe or softpipe).
///
/// # Errors
/// What it is instead.
pub fn judge(vk: Option<&str>, gl: Option<&str>) -> Result<String, String> {
    let vk = vk.ok_or("vulkaninfo named no device")?;
    if !vk.starts_with("Virtio-GPU Venus (Apple") {
        return Err(format!("Vulkan is on {vk}, not Venus over Apple's GPU"));
    }
    let gl = gl.ok_or("glxinfo named no renderer")?;
    if !(gl.starts_with("zink") && gl.contains("Venus (Apple")) {
        return Err(format!("GL is on {gl}, not zink over Venus"));
    }
    Ok(format!("Vulkan {vk}; GL {gl}"))
}

/// The two probes [`judge`] reads, as guest argv: `vulkaninfo --summary`, and `glxinfo -B` on a
/// scratch Xvfb, each under [`gpu_env`].
#[must_use]
pub fn probes() -> [Vec<String>; 2] {
    let env = |argv: &[&str]| {
        crate::linux_guest::with_env(
            &gpu_env(),
            &argv.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        )
    };
    [
        env(&["timeout", "60", "vulkaninfo", "--summary"]),
        env(&[
            "timeout",
            "60",
            "xvfb-run",
            "-a",
            "-s",
            "-screen 0 64x64x24",
            "glxinfo",
            "-B",
        ]),
    ]
}

/// Prove the instance is on this Mac's GPU before anything is measured on it.
///
/// # Errors
/// NOT RUN: a probe failed, or the guest's Vulkan or GL is not the GPU's.
pub fn prove(sh: &dyn Fn(&[String]) -> crate::linux::Out) -> Result<(String, f64), Fail> {
    let t0 = Instant::now();
    let [vk_argv, gl_argv] = probes();
    let vk = sh(&vk_argv);
    let gl = sh(&gl_argv);
    judge(vulkan_device(&vk.text), gl_renderer(&gl.text))
        .map(|named| (named, t0.elapsed().as_secs_f64()))
        .map_err(|why| {
            crate::linux::not_run(format!(
                "the GPU instance is not on this Mac's GPU: {why}\nvulkaninfo: {}\nglxinfo: {}",
                vk.tail(4),
                gl.tail(4)
            ))
        })
}

/// Where macOS writes this user's crash reports (`<process>-<date>.ips`), under `$HOME`.
pub const CRASH_REPORTS: &str = "Library/Logs/DiagnosticReports";

/// How long [`krunkit_lost`] waits for macOS's report of a krunkit that went down: ReportCrash
/// writes it after the process is gone, and a VM stopped for any other reason leaves none.
pub const REPORT_WAIT: Duration = Duration::from_secs(60);

/// What a crash report (`.ips`: a header line, then the report's JSON) says ended its process:
/// the exception's signal and where its faulting thread was, the image beside each symbol. The
/// abort measured on this instance (docs/measured/linux-realgpu-2026-10-24.md, eight reports) is
/// named as that: virglrenderer 0.10.4e freeing a pointer it never allocated in
/// `venus_context_destroy`. `None`: not a crash report.
#[must_use]
pub fn crash_cause(ips: &str) -> Option<String> {
    use aterm_json::Value;
    let (_header, body) = ips.split_once('\n')?;
    let report = aterm_json::from_str::<Value>(body).ok()?;
    let signal = report
        .get("exception")
        .and_then(|e| e.get("signal"))
        .and_then(Value::as_str)
        .unwrap_or("no signal named");
    let images = report.get("usedImages").and_then(Value::as_array);
    let thread = usize::try_from(report.get("faultingThread")?.as_u64()?).ok()?;
    let frames: Vec<(&str, &str)> = report
        .get("threads")?
        .as_array()?
        .get(thread)?
        .get("frames")?
        .as_array()?
        .iter()
        .map(|f| {
            let image = f
                .get("imageIndex")
                .and_then(Value::as_u64)
                .and_then(|i| images?.get(usize::try_from(i).ok()?))
                .and_then(|i| i.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            (
                f.get("symbol").and_then(Value::as_str).unwrap_or("?"),
                image,
            )
        })
        .collect();
    let has = |symbol: &str| frames.iter().any(|(s, _)| s.contains(symbol));
    if has("venus_context_destroy") && has("POINTER_BEING_FREED_WAS_NOT_ALLOCATED") {
        return Some(format!(
            "virglrenderer's double free in venus_context_destroy ({signal}), the abort of \
             virglrenderer 0.10.4e measured in docs/measured/linux-realgpu-2026-10-24.md"
        ));
    }
    let at = frames
        .iter()
        .find(|(_, image)| !image.starts_with("libsystem_"))
        .or_else(|| frames.first())
        .map_or_else(
            || "no frame".to_string(),
            |(symbol, image)| format!("{symbol} ({image})"),
        );
    Some(format!("{signal} in {at}"))
}

/// The newest krunkit crash report in `dir` written at or after `since`, and its text.
#[must_use]
pub fn krunkit_report(dir: &Path, since: SystemTime) -> Option<(PathBuf, String)> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("krunkit-") && name.ends_with(".ips")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(written, _)| *written >= since)
        .max_by_key(|(written, _)| *written)
        .and_then(|(_, path)| Some((path.clone(), crate::util::read_regular_text(&path).ok()?)))
}

/// Why the GPU instance went down under a command begun at `since`: the krunkit crash report
/// macOS wrote in `dir` since then, by [`crash_cause`], waited for up to `wait`; or, when none
/// came, that the VM stopped with no report.
#[must_use]
pub fn krunkit_lost(dir: &Path, since: SystemTime, wait: Duration) -> String {
    let t0 = Instant::now();
    loop {
        if let Some((path, text)) = krunkit_report(dir, since) {
            let cause = crash_cause(&text).unwrap_or_else(|| "an unreadable report".into());
            return format!("krunkit aborted: {cause} ({})", path.display());
        }
        if t0.elapsed() >= wait {
            return format!(
                "the GPU VM stopped, and macOS wrote no krunkit crash report in {} s",
                wait.as_secs()
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
#[path = "linux_gpu_tests.rs"]
mod tests;
