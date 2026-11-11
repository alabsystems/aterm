// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationName {
    pub general: String,
    pub instance: String,
}

impl ApplicationName {
    #[cfg(wayland_platform)]
    pub fn new(general: String, instance: String) -> Self {
        Self { general, instance }
    }
}

#[derive(Clone, Default)]
pub struct PlatformSpecificWindowAttributes {
    pub name: Option<ApplicationName>,
    /// The launcher's startup-notification token (`DESKTOP_STARTUP_ID` on X11, `XDG_ACTIVATION_TOKEN` on Wayland).
    pub activation_token: Option<String>,
    /// The X11 window's background (`0x00RRGGBB`) and top-left gravity.
    pub x11_background: Option<u32>,
}

impl std::fmt::Debug for PlatformSpecificWindowAttributes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformSpecificWindowAttributes")
            .field("name", &self.name)
            .field("activation_token", &self.activation_token.is_some())
            .field("x11_background", &self.x11_background)
            .finish()
    }
}

#[cfg(test)]
#[path = "attributes_tests.rs"]
mod tests;
