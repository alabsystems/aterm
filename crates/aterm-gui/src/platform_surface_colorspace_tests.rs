// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SurfaceColorspace, capture_space_after_surface_tag, resolve_surface_colorspace};
use crate::app_config::WindowColorspace;

#[test]
fn config_maps_one_to_one_and_names_are_the_cg_constants() {
    assert_eq!(
        SurfaceColorspace::from(WindowColorspace::Srgb),
        SurfaceColorspace::Srgb
    );
    assert_eq!(
        SurfaceColorspace::from(WindowColorspace::DisplayP3),
        SurfaceColorspace::DisplayP3
    );
    assert_eq!(SurfaceColorspace::Srgb.cg_name(), "kCGColorSpaceSRGB");
    assert_eq!(
        SurfaceColorspace::DisplayP3.cg_name(),
        "kCGColorSpaceDisplayP3"
    );
    assert_eq!(
        SurfaceColorspace::ExtendedLinearSrgb.cg_name(),
        "kCGColorSpaceExtendedLinearSRGB"
    );
}

#[test]
fn hdr_surface_outranks_config_tag() {
    for cfg in [WindowColorspace::Srgb, WindowColorspace::DisplayP3] {
        assert_eq!(
            resolve_surface_colorspace(cfg, true),
            SurfaceColorspace::ExtendedLinearSrgb,
            "{cfg:?}: an f16 swapchain must be read as extended-linear"
        );
        assert_eq!(
            resolve_surface_colorspace(cfg, false),
            SurfaceColorspace::from(cfg),
            "{cfg:?}: an SDR swapchain follows the config"
        );
    }
}

#[test]
fn failed_surface_tag_never_claims_the_requested_space() {
    use aterm_gpu::video_tap::CaptureColorSpace;

    assert_eq!(
        capture_space_after_surface_tag(None, CaptureColorSpace::Unknown),
        CaptureColorSpace::Unknown,
        "initial best-effort failure must make exact capture refuse"
    );
    assert_eq!(
        capture_space_after_surface_tag(None, CaptureColorSpace::Srgb),
        CaptureColorSpace::Srgb,
        "failed hot retag leaves the prior compositor tag in effect"
    );
    assert_eq!(
        capture_space_after_surface_tag(
            Some(SurfaceColorspace::DisplayP3),
            CaptureColorSpace::Srgb,
        ),
        CaptureColorSpace::DisplayP3
    );
}

/// NEGATIVE CONTROL: when CoreGraphics lists zero active displays (clamshell closed or no
/// display connected), `active_display_unavailable_from` refuses with a `Some` reason naming
/// the missing display instead of letting a window drive judge an empty monitor list.
#[test]
fn active_display_unavailable_names_closed_clamshell_and_passes_active_panel() {
    assert_eq!(
        super::active_display_unavailable_from(Ok(&[1]), Ok(&[1])),
        None,
        "an active display clears the gate"
    );
    assert_eq!(
        super::active_display_unavailable_from(Ok(&[1, 2]), Ok(&[2])),
        None,
        "clamshell closed with an external active display clears the gate"
    );
    let closed = super::active_display_unavailable_from(Ok(&[1]), Ok(&[]))
        .expect("clamshell closed with no external active display refuses");
    assert!(
        closed.contains("no display is online for drawing")
            && closed.contains("no active display")
            && closed.contains("1 online display"),
        "{closed}"
    );
    let headless = super::active_display_unavailable_from(Ok(&[]), Ok(&[]))
        .expect("no online display at all refuses");
    assert!(
        headless.contains("no display is online") && headless.contains("no online display"),
        "{headless}"
    );
    let err = super::active_display_unavailable_from(Ok(&[1]), Err(1000))
        .expect("active display query error refuses");
    assert!(err.contains("CGError 1000"), "{err}");
}
