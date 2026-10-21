// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/randr.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (RandR's requests are
// aterm-x11's encoders, and `Xft.dpi` comes from aterm-x11's resource database, where
// upstream used x11rb's.)

use std::str::FromStr;
use std::{env, str};

use super::*;
use crate::dpi::validate_scale_factor;

use aterm_x11::proto::randr;
use tracing::warn;

/// Represents values of `WINIT_HIDPI_FACTOR`.
pub enum EnvVarDPI {
    Randr,
    Scale(f64),
    NotSet,
}

pub fn calc_dpi_factor(
    (width_px, height_px): (u32, u32),
    (width_mm, height_mm): (u64, u64),
) -> f64 {
    // See http://xpra.org/trac/ticket/728 for more information.
    if width_mm == 0 || height_mm == 0 {
        warn!("XRandR reported that the display's 0mm in size, which is certifiably insane");
        return 1.0;
    }

    let ppmm = ((width_px as f64 * height_px as f64) / (width_mm as f64 * height_mm as f64)).sqrt();
    // Quantize 1/12 step size
    let dpi_factor = ((ppmm * (12.0 * 25.4 / 96.0)).round() / 12.0).max(1.0);
    assert!(validate_scale_factor(dpi_factor));
    if dpi_factor <= 20. {
        dpi_factor
    } else {
        1.
    }
}

impl XConnection {
    // Retrieve DPI from Xft.dpi property
    pub fn get_xft_dpi(&self) -> Option<f64> {
        // Try to get it from XSETTINGS first.
        if let Some(xsettings_screen) = self.xsettings_screen() {
            match self.xsettings_dpi(xsettings_screen) {
                Ok(Some(dpi)) => return Some(dpi),
                Ok(None) => {},
                Err(_err) => {
                    tracing::warn!("failed to fetch XSettings: {_err}");
                },
            }
        }

        self.database().get_string("Xft.dpi", "").and_then(|s| f64::from_str(s).ok())
    }

    pub fn get_output_info(&self, crtc: &randr::GetCrtcInfoReply) -> Option<(String, f64)> {
        let output_info = match self.request(randr::get_output_info(
            self.randr_major(),
            crtc.outputs[0],
            aterm_x11::proto::CURRENT_TIME,
        )) {
            Ok(output_info) => output_info,
            Err(_err) => {
                warn!("Failed to get output info: {:?}", _err);
                return None;
            },
        };

        let name = match str::from_utf8(&output_info.name) {
            Ok(name) => name.to_owned(),
            Err(_err) => {
                warn!("Failed to get output name: {:?}", _err);
                return None;
            },
        };
        // Override DPI if `WINIT_X11_SCALE_FACTOR` variable is set
        let deprecated_dpi_override = env::var("WINIT_HIDPI_FACTOR").ok();
        if deprecated_dpi_override.is_some() {
            warn!(
                "The WINIT_HIDPI_FACTOR environment variable is deprecated; use \
                 WINIT_X11_SCALE_FACTOR"
            )
        }
        let dpi_env = env::var("WINIT_X11_SCALE_FACTOR").ok().map_or_else(
            || EnvVarDPI::NotSet,
            |var| {
                if var.to_lowercase() == "randr" {
                    EnvVarDPI::Randr
                } else if let Ok(dpi) = f64::from_str(&var) {
                    EnvVarDPI::Scale(dpi)
                } else if var.is_empty() {
                    EnvVarDPI::NotSet
                } else {
                    panic!(
                        "`WINIT_X11_SCALE_FACTOR` invalid; DPI factors must be either normal \
                         floats greater than 0, or `randr`. Got `{var}`"
                    );
                }
            },
        );

        let scale_factor = match dpi_env {
            EnvVarDPI::Randr => calc_dpi_factor(
                (crtc.width.into(), crtc.height.into()),
                (output_info.mm_width as _, output_info.mm_height as _),
            ),
            EnvVarDPI::Scale(dpi_override) => {
                if !validate_scale_factor(dpi_override) {
                    panic!(
                        "`WINIT_X11_SCALE_FACTOR` invalid; DPI factors must be either normal \
                         floats greater than 0, or `randr`. Got `{dpi_override}`",
                    );
                }
                dpi_override
            },
            EnvVarDPI::NotSet => {
                if let Some(dpi) = self.get_xft_dpi() {
                    dpi / 96.
                } else {
                    calc_dpi_factor(
                        (crtc.width.into(), crtc.height.into()),
                        (output_info.mm_width as _, output_info.mm_height as _),
                    )
                }
            },
        };

        Some((name, scale_factor))
    }
}
