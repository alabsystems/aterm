// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide code signing identity, OS version, and install posture readers.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aterm_containment::consent::{self, DrClass, plist_string, read_plist_text};

/// Code signing identity to which macOS keys a TCC grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SigningIdentity {
    /// `CFBundleIdentifier` of the running bundle, if any.
    pub(crate) bundle_id: Option<String>,
    /// Subject identifier accepted by `tccutil reset` from the bundle's `Info.plist`.
    pub(crate) reset_subject: Option<consent::ResetSubject>,
    /// `CFBundleDisplayName` or `CFBundleName`, falling back to the executable basename.
    pub(crate) display_name: Option<String>,
    /// Signing class (`developer-id`, `adhoc`, `unsigned`, or `unknown`).
    pub(crate) signing: &'static str,
    /// Apple Team ID when present on the signature.
    pub(crate) team: Option<String>,
    /// Designated requirement classification.
    pub(crate) dr: DrClass,
    /// Whether the bundle carries the `ATermDevBuild` marker.
    pub(crate) dev_build: Option<bool>,
}

impl SigningIdentity {
    /// Default unknown identity used outside a `.app` bundle or before warming.
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

fn running_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().unwrap_or_default())
}

/// Returns the process's signing identity, reading it on first call.
pub(crate) fn signing_identity() -> &'static SigningIdentity {
    identity_cell().get_or_init(read_signing_identity)
}

/// Returns the cached signing identity if already warmed, or [`SigningIdentity::unknown`].
pub(crate) fn signing_identity_if_warm() -> &'static SigningIdentity {
    identity_cell()
        .get()
        .unwrap_or_else(SigningIdentity::unknown)
}

/// Returns whether the signing identity has been warmed.
pub(crate) fn signing_identity_is_warm() -> bool {
    identity_cell().get().is_some()
}

/// Returns the cached bundle identifier if the signing identity is warm.
pub(crate) fn running_bundle_id_if_warm() -> Option<&'static str> {
    signing_identity_if_warm().bundle_id.as_deref()
}

/// Returns the cached designated requirement class, or [`DrClass::Unknown`] if not warm.
pub(crate) fn running_dr_class_if_warm() -> DrClass {
    signing_identity_if_warm().dr
}

/// Returns whether the running executable resides inside a `.app` bundle.
pub(crate) fn running_from_bundle() -> bool {
    static BUNDLED: OnceLock<bool> = OnceLock::new();
    *BUNDLED.get_or_init(|| consent::app_bundle_root(running_exe()).is_some())
}

fn identity_cell() -> &'static OnceLock<SigningIdentity> {
    static IDENTITY: OnceLock<SigningIdentity> = OnceLock::new();
    &IDENTITY
}

/// Returns the running bundle's `CFBundleIdentifier`, warming the identity if needed.
pub(crate) fn running_bundle_id() -> Option<&'static str> {
    signing_identity().bundle_id.as_deref()
}

fn read_signing_identity() -> SigningIdentity {
    let exe = running_exe();
    let Some(root) = consent::app_bundle_root(exe) else {
        return SigningIdentity::unknown().clone();
    };
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
        #[cfg(target_os = "macos")]
        dev_build: text.as_deref().map(aterm_update::plist_marks_dev_build),
        #[cfg(not(target_os = "macos"))]
        dev_build: text.as_deref().map(|_| false),
    }
}

/// Returns the host macOS `ProductVersion` read once from `SystemVersion.plist`.
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

/// Returns the cached `(install_token, running_exe_path)` posture pair.
pub(crate) fn install_posture_once() -> &'static (&'static str, Option<String>) {
    static ONCE: std::sync::OnceLock<(&'static str, Option<String>)> = std::sync::OnceLock::new();
    ONCE.get_or_init(install_posture_rows)
}

fn install_posture_rows() -> (&'static str, Option<String>) {
    let Ok(exe) = std::env::current_exe() else {
        return ("unknown", None);
    };
    #[cfg(unix)]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
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
