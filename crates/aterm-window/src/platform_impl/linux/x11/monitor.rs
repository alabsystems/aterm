// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/monitor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (RandR's requests are
// aterm-x11's encoders, where upstream used x11rb's, and a broken connection has no monitors.)

use super::{util, IgnoreError as _, X11Error, XConnection};
use crate::dpi::{PhysicalPosition, PhysicalSize};
use aterm_x11::proto::{randr, xproto};

const DISABLE_MONITOR_LIST_CACHING: bool = false;

impl XConnection {
    pub fn invalidate_cached_monitor_list(&self) -> Option<Vec<MonitorHandle>> {
        self.monitor_handles.lock().unwrap().take()
    }
}

#[derive(Debug, Clone)]
pub struct MonitorHandle {
    /// The actual id
    pub(crate) id: randr::Crtc,
    /// The name of the monitor: which monitor a RandR change moved the scale of (WC.8: no
    /// longer answered by `MonitorHandle::name`, which no aterm crate asks for on Linux).
    pub(crate) name: String,
    /// The size of the monitor
    dimensions: (u32, u32),
    /// The position of the monitor in the X screen
    position: (i32, i32),
    /// If the monitor is the primary one
    primary: bool,
    /// The refresh rate used by monitor.
    refresh_rate_millihertz: Option<u32>,
    /// The DPI scale factor
    pub(crate) scale_factor: f64,
    /// Used to determine which windows are on this monitor
    pub(crate) rect: util::AaRect,
}

impl PartialEq for MonitorHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for MonitorHandle {}

impl PartialOrd for MonitorHandle {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MonitorHandle {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

impl std::hash::Hash for MonitorHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[inline]
pub fn mode_refresh_rate_millihertz(mode: &randr::ModeInfo) -> Option<u32> {
    if mode.dot_clock > 0 && mode.htotal > 0 && mode.vtotal > 0 {
        #[allow(clippy::unnecessary_cast)]
        Some((mode.dot_clock as u64 * 1000 / (mode.htotal as u64 * mode.vtotal as u64)) as u32)
    } else {
        None
    }
}

impl MonitorHandle {
    fn new(
        xconn: &XConnection,
        resources: &ScreenResources,
        id: randr::Crtc,
        crtc: &randr::GetCrtcInfoReply,
        primary: bool,
    ) -> Option<Self> {
        let (name, scale_factor) = xconn.get_output_info(crtc)?;
        let dimensions = (crtc.width as u32, crtc.height as u32);
        let position = (crtc.x as i32, crtc.y as i32);

        let current_mode = crtc.mode;
        let screen_modes = resources.modes();
        let refresh_rate_millihertz = screen_modes
            .iter()
            .find(|mode| mode.id == current_mode)
            .and_then(mode_refresh_rate_millihertz);

        let rect = util::AaRect::new(position, dimensions);

        Some(MonitorHandle {
            id,
            name,
            refresh_rate_millihertz,
            scale_factor,
            dimensions,
            position,
            primary,
            rect,
        })
    }

    pub fn dummy() -> Self {
        MonitorHandle {
            id: 0,
            name: "<dummy monitor>".into(),
            scale_factor: 1.0,
            dimensions: (1, 1),
            position: (0, 0),
            refresh_rate_millihertz: None,
            primary: true,
            rect: util::AaRect::new((0, 0), (1, 1)),
        }
    }

    pub(crate) fn is_dummy(&self) -> bool {
        self.id == 0
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.dimensions.into()
    }

    pub fn position(&self) -> PhysicalPosition<i32> {
        self.position.into()
    }

    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        self.refresh_rate_millihertz
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }
}

impl XConnection {
    pub fn get_monitor_for_window(
        &self,
        window_rect: Option<util::AaRect>,
    ) -> Result<MonitorHandle, X11Error> {
        let monitors = self.available_monitors()?;

        if monitors.is_empty() {
            return Ok(MonitorHandle::dummy());
        }

        let default = monitors.first().unwrap();

        let window_rect = match window_rect {
            Some(rect) => rect,
            None => return Ok(default.to_owned()),
        };

        let mut largest_overlap = 0;
        let mut matched_monitor = default;
        for monitor in &monitors {
            let overlapping_area = window_rect.get_overlapping_area(&monitor.rect);
            if overlapping_area > largest_overlap {
                largest_overlap = overlapping_area;
                matched_monitor = monitor;
            }
        }

        Ok(matched_monitor.to_owned())
    }

    fn query_monitor_list(&self) -> Result<Vec<MonitorHandle>, X11Error> {
        let root = self.default_root();
        let major = self.randr_major();
        let resources = ScreenResources::from_connection(self, root, self.randr_version())?;

        let mut crtc_cookies = Vec::with_capacity(resources.crtcs().len());
        for &crtc in resources.crtcs() {
            crtc_cookies.push(self.xcb_connection().send(randr::get_crtc_info(
                major,
                crtc,
                aterm_x11::proto::CURRENT_TIME,
            ))?);
        }

        let primary = self.request(randr::get_output_primary(major, root.root))?.output;

        let mut crtc_infos = Vec::with_capacity(crtc_cookies.len());
        for cookie in crtc_cookies {
            let reply = cookie.reply()?;
            crtc_infos.push(reply);
        }

        let mut has_primary = false;
        let mut available_monitors = Vec::with_capacity(resources.crtcs().len());
        for (crtc_id, crtc) in resources.crtcs().iter().zip(crtc_infos.iter()) {
            if crtc.width == 0 || crtc.height == 0 || crtc.outputs.is_empty() {
                continue;
            }

            let is_primary = crtc.outputs[0] == primary;
            has_primary |= is_primary;
            let monitor = MonitorHandle::new(self, &resources, *crtc_id, crtc, is_primary);
            available_monitors.extend(monitor);
        }

        if !has_primary {
            if let Some(ref mut fallback) = available_monitors.first_mut() {
                fallback.primary = true;
            }
        }

        Ok(available_monitors)
    }

    /// The server's monitors, cached until RandR reports a change. Once the connection is broken
    /// the cache describes a server that has gone, so the list is asked for again and that
    /// request's failure is the answer: no monitor to place or scale a window by.
    pub fn available_monitors(&self) -> Result<Vec<MonitorHandle>, X11Error> {
        let mut monitors_lock = self.monitor_handles.lock().unwrap();
        match *monitors_lock {
            Some(ref monitors) if !self.is_broken() => Ok(monitors.clone()),
            _ => {
                let monitors = self.query_monitor_list()?;
                if !DISABLE_MONITOR_LIST_CACHING {
                    *monitors_lock = Some(monitors.clone());
                }
                Ok(monitors)
            },
        }
    }

    #[inline]
    pub fn primary_monitor(&self) -> Result<MonitorHandle, X11Error> {
        Ok(self
            .available_monitors()?
            .into_iter()
            .find(|monitor| monitor.primary)
            .unwrap_or_else(MonitorHandle::dummy))
    }

    pub fn select_xrandr_input(&self, root: xproto::Window) -> Result<u8, X11Error> {
        use randr::notify_mask;

        let info = self
            .xcb_connection()
            .extension(randr::NAME)?
            .ok_or(X11Error::MissingExtension("RANDR"))?;

        let event_mask =
            notify_mask::CRTC_CHANGE | notify_mask::OUTPUT_PROPERTY | notify_mask::SCREEN_CHANGE;
        self.send_void(randr::select_input(info.major_opcode, root, event_mask))?.ignore_error();

        Ok(info.first_event)
    }
}

pub struct ScreenResources {
    /// List of attached modes.
    modes: Vec<randr::ModeInfo>,
    /// List of attached CRTCs.
    crtcs: Vec<randr::Crtc>,
}

impl ScreenResources {
    pub(crate) fn modes(&self) -> &[randr::ModeInfo] {
        &self.modes
    }

    pub(crate) fn crtcs(&self) -> &[randr::Crtc] {
        &self.crtcs
    }

    pub(crate) fn from_connection(
        conn: &XConnection,
        root: &xproto::Screen,
        (major_version, minor_version): (u32, u32),
    ) -> Result<Self, X11Error> {
        let major = conn.randr_major();
        let reply = if (major_version == 1 && minor_version >= 3) || major_version > 1 {
            conn.request(randr::get_screen_resources_current(major, root.root))?
        } else {
            conn.request(randr::get_screen_resources(major, root.root))?
        };
        Ok(Self::from_screen_resources_reply(reply))
    }

    /// GetScreenResources' and GetScreenResourcesCurrent's reply, one layout.
    pub(crate) fn from_screen_resources_reply(reply: randr::ScreenResourcesReply) -> Self {
        Self { modes: reply.modes, crtcs: reply.crtcs }
    }
}
