// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The code identity a TCC grant is keyed to, and the other process-wide facts
//! the consent readers take once: the host OS version and the install posture.
//! None of it is a consent surface or a `WindowServer` contact.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aterm_containment::consent::{self, DrClass, plist_string, read_plist_text};

/// The code identity macOS keys a TCC grant to. Read ONCE per process: the
/// running binary's identity cannot change under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SigningIdentity {
    /// `CFBundleIdentifier` of the running bundle, when there is one.
    pub(crate) bundle_id: Option<String>,
    /// The same id as the one subject a `tccutil reset` may name, read from
    /// the running bundle's `Info.plist` ([`consent::ResetSubject::read`]).
    pub(crate) reset_subject: Option<consent::ResetSubject>,
    /// `CFBundleDisplayName` / `CFBundleName`, else the executable basename.
    pub(crate) display_name: Option<String>,
    /// `developer-id` | `adhoc` | `unsigned` | `unknown`.
    pub(crate) signing: &'static str,
    /// The Team ID, when the signature carries one.
    pub(crate) team: Option<String>,
    /// The designated requirement's class.
    pub(crate) dr: DrClass,
    /// Whether the bundle carries the `ATermDevBuild` mark.
    pub(crate) dev_build: Option<bool>,
}

impl SigningIdentity {
    /// Everything unknown — what a caller outside a `.app` reports, and the
    /// value the event loop reads while the identity is not yet warm.
    pub(crate) fn unknown() -> &'static Self {
        static UNKNOWN: OnceLock<SigningIdentity> = OnceLock::new();
        UNKNOWN.get_or_init(|| Self {
            bundle_id: None,
            reset_subject: None,
            display_name: None,
            signing: "unknown",
            team: None,
            dr: DrClass::Unknown,
            dev_build: None,
        })
    }
}

/// The running executable, resolved once. Pure path resolution — no consent
/// surface, no `WindowServer`.
fn running_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().unwrap_or_default())
}

/// The running process's signing identity, read at most once, on the control
/// thread (`cmd_privacy` warms it before the main-thread hop): it may spawn
/// `codesign`, a 50–150 ms stall. It touches neither `tccd` nor `WindowServer`,
/// and does nothing unless the executable is inside a `.app`.
pub(crate) fn signing_identity() -> &'static SigningIdentity {
    identity_cell().get_or_init(read_signing_identity)
}

/// The identity only if already read. The event loop uses this: until a
/// control-thread verb warmed it, main-thread paths see
/// [`SigningIdentity::unknown`].
pub(crate) fn signing_identity_if_warm() -> &'static SigningIdentity {
    identity_cell()
        .get()
        .unwrap_or_else(SigningIdentity::unknown)
}

/// Whether the identity has been read (off the event loop) by anything yet.
pub(crate) fn signing_identity_is_warm() -> bool {
    identity_cell().get().is_some()
}

/// [`running_bundle_id`] for the event loop: `None` until the identity has
/// been read off it.
pub(crate) fn running_bundle_id_if_warm() -> Option<&'static str> {
    signing_identity_if_warm().bundle_id.as_deref()
}

/// The designated requirement's class for the event loop: `DrClass::Unknown` until
/// the identity has been read off it (which `grant_stable` treats as unstable).
pub(crate) fn running_dr_class_if_warm() -> DrClass {
    signing_identity_if_warm().dr
}

/// Whether the running executable sits inside a `.app` bundle: a pure path
/// test (no file is read, nothing spawns), so the event loop may ask it before
/// the identity is warm. An unbundled run has no bundle id to be a dialog's
/// subject. Answered once, like [`running_exe`].
pub(crate) fn running_from_bundle() -> bool {
    static BUNDLED: OnceLock<bool> = OnceLock::new();
    *BUNDLED.get_or_init(|| consent::app_bundle_root(running_exe()).is_some())
}

fn identity_cell() -> &'static OnceLock<SigningIdentity> {
    static IDENTITY: OnceLock<SigningIdentity> = OnceLock::new();
    &IDENTITY
}

/// The running bundle's `CFBundleIdentifier`, or `None` when the executable is
/// not inside a `.app`, its `Info.plist` names none or cannot be read, or the
/// bundle sits under a protected root, whose `Info.plist` aterm declines to
/// read (`read_signing_identity`). Callers omit the claim on `None`; never
/// substitute the release channel's id (design §3.1). It reads the identity
/// when nothing has yet, so never on the event loop.
pub(crate) fn running_bundle_id() -> Option<&'static str> {
    signing_identity().bundle_id.as_deref()
}

fn read_signing_identity() -> SigningIdentity {
    let exe = running_exe();
    let Some(root) = consent::app_bundle_root(exe) else {
        return SigningIdentity::unknown().clone();
    };
    // An `Info.plist` under a protected root is exactly the read this design
    // exists to avoid, so both reads refuse it and the identity is `unknown`.
    let protected = consent::protected_roots(&[]);
    let Some(identity) = consent::bundle_identity(&root, &protected) else {
        return SigningIdentity::unknown().clone();
    };
    let text = consent::readable_info_plist(exe, &protected)
        .as_deref()
        .and_then(read_plist_text);
    let display_name = text
        .as_deref()
        .and_then(|t| plist_string(t, "CFBundleDisplayName").or(plist_string(t, "CFBundleName")))
        .map(str::to_owned);
    SigningIdentity {
        reset_subject: consent::ResetSubject::read(&root, &protected),
        bundle_id: identity.bundle_id,
        display_name,
        signing: identity.signing,
        team: identity.team,
        dr: identity.dr,
        // The updater's own reading of the `ATermDevBuild` mark (fails open).
        #[cfg(target_os = "macos")]
        dev_build: text.as_deref().map(aterm_update::plist_marks_dev_build),
        // No `.app` bundle exists off macOS, so no build is marked there.
        #[cfg(not(target_os = "macos"))]
        dev_build: text.as_deref().map(|_| false),
    }
}

/// The host OS version (`ProductVersion`), read once from the public system
/// plist. Not a consent surface and not a `WindowServer` contact: an ordinary
/// read of a world-readable file under `/System`.
pub(crate) fn os_version() -> Option<&'static str> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            if !cfg!(target_os = "macos") {
                return None;
            }
            let text = read_plist_text(Path::new(
                "/System/Library/CoreServices/SystemVersion.plist",
            ))?;
            plist_string(&text, "ProductVersion").map(str::to_owned)
        })
        .as_deref()
}

/// [`install_posture_rows`], computed once: the executable path of a running
/// process does not change, and the Security panel reads this on every park
/// while it is open.
pub(crate) fn install_posture_once() -> &'static (&'static str, Option<String>) {
    static ONCE: std::sync::OnceLock<(&'static str, Option<String>)> = std::sync::OnceLock::new();
    ONCE.get_or_init(install_posture_rows)
}

/// The `install=` token and `running=` path: the first-open doctor's
/// classification (`aterm_update::which_copy::posture_from`) of this process's
/// canonical executable path on macOS; `not-a-bundle` elsewhere. No TCC.
/// Canonicalized on unix only: Windows answers a verbatim `\\?\C:\…` path.
fn install_posture_rows() -> (&'static str, Option<String>) {
    let Ok(exe) = std::env::current_exe() else {
        return ("unknown", None);
    };
    #[cfg(unix)]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    // `which_copy::posture_from` is macOS-only (translocation and mounted
    // images are Gatekeeper/DMG phenomena). Elsewhere the running copy is a
    // plain binary: `InstallPosture::NotABundle`.
    #[cfg(target_os = "macos")]
    let token = match aterm_update::which_copy::posture_from(&exe) {
        aterm_update::which_copy::InstallPosture::Installed => "installed",
        aterm_update::which_copy::InstallPosture::MountedImage => "mounted-image",
        aterm_update::which_copy::InstallPosture::Translocated => "translocated",
        aterm_update::which_copy::InstallPosture::NotABundle => "not-a-bundle",
    };
    #[cfg(not(target_os = "macos"))]
    let token = "not-a-bundle";
    (token, Some(exe.to_string_lossy().into_owned()))
}
