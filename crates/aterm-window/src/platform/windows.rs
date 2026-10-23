// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/windows.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! # Windows

/// aterm: recorded Windows key messages, replayed through this backend's real key path, for the tests of the crates above it (`aterm-gui`'s byte-level layout tests).
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub use crate::platform_impl::recorded;

use std::borrow::Borrow;
use std::ffi::c_void;
use std::path::Path;

use crate::dpi::PhysicalSize;
use crate::event::DeviceId;
use crate::event_loop::EventLoopBuilder;
use crate::monitor::MonitorHandle;
use crate::window::{BadIcon, Icon, Window, WindowAttributes};

/// Window Handle type used by Win32 API
pub type HWND = isize;
/// Menu Handle type used by Win32 API
pub type HMENU = isize;
/// Monitor Handle type used by Win32 API
pub type HMONITOR = isize;

/// Describes a system-drawn backdrop material of a window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackdropType {
    /// Corresponds to `DWMSBT_AUTO`.
    #[default]
    Auto = 0,

    /// Corresponds to `DWMSBT_NONE`.
    None = 1,

    /// Corresponds to `DWMSBT_MAINWINDOW`.
    MainWindow = 2,

    /// Corresponds to `DWMSBT_TRANSIENTWINDOW`.
    TransientWindow = 3,

    /// Corresponds to `DWMSBT_TABBEDWINDOW`.
    TabbedWindow = 4,
}

/// Describes a color used by Windows
#[repr(transparent)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Color(u32);

impl Color {
    const NONE: Color = Color(0xfffffffe);
    /// Use the system's default color
    pub const SYSTEM_DEFAULT: Color = Color(0xffffffff);

    /// Create a new color from the given RGB values
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
    }
}

impl Default for Color {
    fn default() -> Self {
        Self::SYSTEM_DEFAULT
    }
}

/// Describes how the corners of a window should look like.
#[repr(i32)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CornerPreference {
    /// Corresponds to `DWMWCP_DEFAULT`.
    #[default]
    Default = 0,

    /// Corresponds to `DWMWCP_DONOTROUND`.
    DoNotRound = 1,

    /// Corresponds to `DWMWCP_ROUND`.
    Round = 2,

    /// Corresponds to `DWMWCP_ROUNDSMALL`.
    RoundSmall = 3,
}

/// A wrapper around a [`Window`] that ignores thread-specific window handle limitations.
#[derive(Debug)]
pub struct AnyThread<W>(W);

impl<W: Borrow<Window>> AnyThread<W> {
    /// Get a reference to the inner window.
    #[inline]
    pub fn get_ref(&self) -> &Window {
        self.0.borrow()
    }

    /// Get a reference to the inner object.
    #[inline]
    pub fn inner(&self) -> &W {
        &self.0
    }

    /// Unwrap and get the inner window.
    #[inline]
    pub fn into_inner(self) -> W {
        self.0
    }
}

impl<W: Borrow<Window>> AsRef<Window> for AnyThread<W> {
    fn as_ref(&self) -> &Window {
        self.get_ref()
    }
}

impl<W: Borrow<Window>> Borrow<Window> for AnyThread<W> {
    fn borrow(&self) -> &Window {
        self.get_ref()
    }
}

impl<W: Borrow<Window>> std::ops::Deref for AnyThread<W> {
    type Target = Window;

    fn deref(&self) -> &Self::Target {
        self.get_ref()
    }
}

#[cfg(feature = "rwh_06")]
impl<W: Borrow<Window>> rwh_06::HasWindowHandle for AnyThread<W> {
    fn window_handle(&self) -> Result<rwh_06::WindowHandle<'_>, rwh_06::HandleError> {
        // SAFETY: The top level user has asserted this is only used safely.
        unsafe { self.get_ref().window_handle_any_thread() }
    }
}

/// Additional methods on `EventLoop` that are specific to Windows.
pub trait EventLoopBuilderExtWindows {
    /// Whether to allow the event loop to be created off of the main thread.
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self;

    /// Whether to enable process-wide DPI awareness.
    ///
    /// # Example
    ///
    /// Disable process-wide DPI awareness.
    ///
    /// ```
    /// use aterm_window::event_loop::EventLoop;
    /// #[cfg(target_os = "windows")]
    /// use aterm_window::platform::windows::EventLoopBuilderExtWindows;
    ///
    /// let mut builder = EventLoop::builder();
    /// #[cfg(target_os = "windows")]
    /// builder.with_dpi_aware(false);
    /// # if false { // We can't test this part
    /// let event_loop = builder.build();
    /// # }
    /// ```
    fn with_dpi_aware(&mut self, dpi_aware: bool) -> &mut Self;

    // The example is compiled code: the win32 oracle (`verify cells`) reads the
    // `windows_sys` names it uses, and aterm-win32 carries the two accelerator
    // functions for it.
    /// A callback to be executed before dispatching a win32 message to the window procedure.
    /// Return true to disable winit's internal message dispatching.
    ///
    /// # Example
    ///
    /// ```
    /// # use windows_sys::Win32::UI::WindowsAndMessaging::{ACCEL, CreateAcceleratorTableW, TranslateAcceleratorW, DispatchMessageW, TranslateMessage, MSG};
    /// use aterm_window::event_loop::EventLoop;
    /// #[cfg(target_os = "windows")]
    /// use aterm_window::platform::windows::EventLoopBuilderExtWindows;
    ///
    /// let mut builder = EventLoop::builder();
    /// #[cfg(target_os = "windows")]
    /// builder.with_msg_hook(|msg|{
    ///     let msg = msg as *const MSG;
    /// #   let accels: Vec<ACCEL> = Vec::new();
    ///     let translated = unsafe {
    ///         TranslateAcceleratorW(
    ///             (*msg).hwnd,
    ///             CreateAcceleratorTableW(accels.as_ptr() as _, 1),
    ///             msg,
    ///         ) == 1
    ///     };
    ///     translated
    /// });
    /// ```
    /// # use windows_sys::Win32::UI::WindowsAndMessaging::{ACCEL, CreateAcceleratorTableW, TranslateAcceleratorW, DispatchMessageW, TranslateMessage, MSG};
    /// use aterm_window::event_loop::EventLoop;
    /// #[cfg(target_os = "windows")]
    /// use aterm_window::platform::windows::EventLoopBuilderExtWindows;
    fn with_msg_hook<F>(&mut self, callback: F) -> &mut Self
    where
        F: FnMut(*const c_void) -> bool + 'static;
}

impl<T> EventLoopBuilderExtWindows for EventLoopBuilder<T> {
    #[inline]
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self {
        self.platform_specific.any_thread = any_thread;
        self
    }

    #[inline]
    fn with_dpi_aware(&mut self, dpi_aware: bool) -> &mut Self {
        self.platform_specific.dpi_aware = dpi_aware;
        self
    }

    #[inline]
    fn with_msg_hook<F>(&mut self, callback: F) -> &mut Self
    where
        F: FnMut(*const c_void) -> bool + 'static,
    {
        self.platform_specific.msg_hook = Some(Box::new(callback));
        self
    }
}

/// Additional methods on `Window` that are specific to Windows.
pub trait WindowExtWindows {
    /// Enables or disables mouse and keyboard input to the specified window.
    fn set_enable(&self, enabled: bool);

    /// This sets `ICON_BIG`. A good ceiling here is 256x256.
    fn set_taskbar_icon(&self, taskbar_icon: Option<Icon>);

    /// Whether to show or hide the window icon in the taskbar.
    fn set_skip_taskbar(&self, skip: bool);

    /// Shows or hides the background drop shadow for undecorated windows.
    fn set_undecorated_shadow(&self, shadow: bool);

    /// Sets system-drawn backdrop type.
    fn set_system_backdrop(&self, backdrop_type: BackdropType);

    /// Sets the color of the window border.
    fn set_border_color(&self, color: Option<Color>);

    /// Sets the background color of the title bar.
    fn set_title_background_color(&self, color: Option<Color>);

    /// Sets the color of the window title.
    fn set_title_text_color(&self, color: Color);

    /// Sets the preferred style of the window corners.
    fn set_corner_preference(&self, preference: CornerPreference);

    /// Get the raw window handle for this [`Window`] without checking for thread affinity.
    /// # Safety
    ///
    /// It is the responsibility of the user to only pass the window handle into thread-safe
    /// Win32 APIs.
    ///
    /// [`SetWindowSubclass`]: https://learn.microsoft.com/en-us/windows/win32/api/commctrl/nf-commctrl-setwindowsubclass
    /// [`GetDC`]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdc
    /// [`Window`]: crate::window::Window
    /// [`window_handle`]: https://docs.rs/raw-window-handle/latest/raw_window_handle/trait.HasWindowHandle.html#tymethod.window_handle
    /// [`Unavailable`]: https://docs.rs/raw-window-handle/latest/raw_window_handle/enum.HandleError.html#variant.Unavailable
    ///
    /// ## Example
    ///
    /// ```no_run
    /// # use aterm_window::window::Window;
    /// # fn scope(window: Window) {
    /// use std::thread;
    /// use aterm_window::platform::windows::WindowExtWindows;
    /// use aterm_window::raw_window_handle::HasWindowHandle;
    ///
    /// // We can get the window handle on the current thread.
    /// let handle = window.window_handle().unwrap();
    ///
    /// // However, on another thread, we can't!
    /// thread::spawn(move || {
    ///     assert!(window.window_handle().is_err());
    ///
    ///     // We can use this function as an escape hatch.
    ///     let handle = unsafe { window.window_handle_any_thread().unwrap() };
    /// });
    /// # }
    /// ```
    #[cfg(feature = "rwh_06")]
    unsafe fn window_handle_any_thread(
        &self,
    ) -> Result<rwh_06::WindowHandle<'_>, rwh_06::HandleError>;
}

impl WindowExtWindows for Window {
    #[inline]
    fn set_enable(&self, enabled: bool) {
        self.window.set_enable(enabled)
    }

    #[inline]
    fn set_taskbar_icon(&self, taskbar_icon: Option<Icon>) {
        self.window.set_taskbar_icon(taskbar_icon)
    }

    #[inline]
    fn set_skip_taskbar(&self, skip: bool) {
        self.window.set_skip_taskbar(skip)
    }

    #[inline]
    fn set_undecorated_shadow(&self, shadow: bool) {
        self.window.set_undecorated_shadow(shadow)
    }

    #[inline]
    fn set_system_backdrop(&self, backdrop_type: BackdropType) {
        self.window.set_system_backdrop(backdrop_type)
    }

    #[inline]
    fn set_border_color(&self, color: Option<Color>) {
        self.window.set_border_color(color.unwrap_or(Color::NONE))
    }

    #[inline]
    fn set_title_background_color(&self, color: Option<Color>) {
        // The windows docs don't mention NONE as a valid options but it works in practice and is
        // useful to circumvent the Windows option "Show accent color on title bars and
        // window borders".
        self.window.set_title_background_color(color.unwrap_or(Color::NONE))
    }

    #[inline]
    fn set_title_text_color(&self, color: Color) {
        self.window.set_title_text_color(color)
    }

    #[inline]
    fn set_corner_preference(&self, preference: CornerPreference) {
        self.window.set_corner_preference(preference)
    }

    #[cfg(feature = "rwh_06")]
    unsafe fn window_handle_any_thread(
        &self,
    ) -> Result<rwh_06::WindowHandle<'_>, rwh_06::HandleError> {
        unsafe {
            let handle = self.window.rwh_06_no_thread_check()?;

            // SAFETY: The handle is valid in this context.
            Ok(rwh_06::WindowHandle::borrow_raw(handle))
        }
    }
}

/// Additional methods for anything that dereference to [`Window`].
pub trait WindowBorrowExtWindows: Borrow<Window> + Sized {
    /// Create an object that allows accessing the inner window handle in a thread-unsafe way.
    /// # Safety
    ///
    /// It is the responsibility of the user to only pass the window handle into thread-safe
    /// Win32 APIs.
    ///
    /// [`window_handle_any_thread`]: WindowExtWindows::window_handle_any_thread
    /// [`Window`]: crate::window::Window
    /// [`HasWindowHandle`]: rwh_06::HasWindowHandle
    unsafe fn any_thread(self) -> AnyThread<Self> {
        AnyThread(self)
    }
}

impl<W: Borrow<Window> + Sized> WindowBorrowExtWindows for W {}

/// Additional methods on `WindowAttributes` that are specific to Windows.
#[allow(rustdoc::broken_intra_doc_links)]
pub trait WindowAttributesExtWindows {
    /// Set an owner to the window to be created. Can be used to create a dialog box, for example.
    fn with_owner_window(self, parent: HWND) -> Self;

    /// Sets a menu on the window to be created.
    #[cfg_attr(
        windows_platform,
        doc = "[`CreateMenu`]: windows_sys::Win32::UI::WindowsAndMessaging::CreateMenu"
    )]
    #[cfg_attr(not(windows_platform), doc = "[`CreateMenu`]: #only-available-on-windows")]
    fn with_menu(self, menu: HMENU) -> Self;

    /// This sets `ICON_BIG`. A good ceiling here is 256x256.
    fn with_taskbar_icon(self, taskbar_icon: Option<Icon>) -> Self;

    /// This sets `WS_EX_NOREDIRECTIONBITMAP`.
    fn with_no_redirection_bitmap(self, flag: bool) -> Self;

    /// Enables or disables drag and drop support (enabled by default).
    fn with_drag_and_drop(self, flag: bool) -> Self;

    /// Whether show or hide the window icon in the taskbar.
    fn with_skip_taskbar(self, skip: bool) -> Self;

    /// Customize the window class name.
    fn with_class_name<S: Into<String>>(self, class_name: S) -> Self;

    /// Shows or hides the background drop shadow for undecorated windows.
    fn with_undecorated_shadow(self, shadow: bool) -> Self;

    /// Sets system-drawn backdrop type.
    fn with_system_backdrop(self, backdrop_type: BackdropType) -> Self;

    /// This sets or removes `WS_CLIPCHILDREN` style.
    fn with_clip_children(self, flag: bool) -> Self;

    /// Sets the color of the window border.
    fn with_border_color(self, color: Option<Color>) -> Self;

    /// Sets the background color of the title bar.
    fn with_title_background_color(self, color: Option<Color>) -> Self;

    /// Sets the color of the window title.
    fn with_title_text_color(self, color: Color) -> Self;

    /// Sets the preferred style of the window corners.
    fn with_corner_preference(self, corners: CornerPreference) -> Self;
}

impl WindowAttributesExtWindows for WindowAttributes {
    #[inline]
    fn with_owner_window(mut self, parent: HWND) -> Self {
        self.platform_specific.owner = Some(crate::platform_impl::handle_from_isize(parent));
        self
    }

    #[inline]
    fn with_menu(mut self, menu: HMENU) -> Self {
        self.platform_specific.menu = Some(crate::platform_impl::handle_from_isize(menu));
        self
    }

    #[inline]
    fn with_taskbar_icon(mut self, taskbar_icon: Option<Icon>) -> Self {
        self.platform_specific.taskbar_icon = taskbar_icon;
        self
    }

    #[inline]
    fn with_no_redirection_bitmap(mut self, flag: bool) -> Self {
        self.platform_specific.no_redirection_bitmap = flag;
        self
    }

    #[inline]
    fn with_drag_and_drop(mut self, flag: bool) -> Self {
        self.platform_specific.drag_and_drop = flag;
        self
    }

    #[inline]
    fn with_skip_taskbar(mut self, skip: bool) -> Self {
        self.platform_specific.skip_taskbar = skip;
        self
    }

    #[inline]
    fn with_class_name<S: Into<String>>(mut self, class_name: S) -> Self {
        self.platform_specific.class_name = class_name.into();
        self
    }

    #[inline]
    fn with_undecorated_shadow(mut self, shadow: bool) -> Self {
        self.platform_specific.decoration_shadow = shadow;
        self
    }

    #[inline]
    fn with_system_backdrop(mut self, backdrop_type: BackdropType) -> Self {
        self.platform_specific.backdrop_type = backdrop_type;
        self
    }

    #[inline]
    fn with_clip_children(mut self, flag: bool) -> Self {
        self.platform_specific.clip_children = flag;
        self
    }

    #[inline]
    fn with_border_color(mut self, color: Option<Color>) -> Self {
        self.platform_specific.border_color = Some(color.unwrap_or(Color::NONE));
        self
    }

    #[inline]
    fn with_title_background_color(mut self, color: Option<Color>) -> Self {
        self.platform_specific.title_background_color = Some(color.unwrap_or(Color::NONE));
        self
    }

    #[inline]
    fn with_title_text_color(mut self, color: Color) -> Self {
        self.platform_specific.title_text_color = Some(color);
        self
    }

    #[inline]
    fn with_corner_preference(mut self, corners: CornerPreference) -> Self {
        self.platform_specific.corner_preference = Some(corners);
        self
    }
}

/// Additional methods on `MonitorHandle` that are specific to Windows.
pub trait MonitorHandleExtWindows {
    /// Returns the name of the monitor adapter specific to the Win32 API.
    fn native_id(&self) -> String;

    /// Returns the handle of the monitor - `HMONITOR`.
    fn hmonitor(&self) -> HMONITOR;
}

impl MonitorHandleExtWindows for MonitorHandle {
    #[inline]
    fn native_id(&self) -> String {
        self.inner.native_identifier()
    }

    #[inline]
    fn hmonitor(&self) -> HMONITOR {
        crate::platform_impl::handle_to_isize(self.inner.hmonitor())
    }
}

/// Additional methods on `DeviceId` that are specific to Windows.
pub trait DeviceIdExtWindows {
    /// Returns an identifier that persistently refers to this specific device.
    fn persistent_identifier(&self) -> Option<String>;
}

impl DeviceIdExtWindows for DeviceId {
    #[inline]
    fn persistent_identifier(&self) -> Option<String> {
        self.0.persistent_identifier()
    }
}

/// Additional methods on `Icon` that are specific to Windows.
pub trait IconExtWindows: Sized {
    /// Create an icon from a file path.
    fn from_path<P: AsRef<Path>>(path: P, size: Option<PhysicalSize<u32>>)
        -> Result<Self, BadIcon>;

    /// Create an icon from a resource embedded in this executable or library by its ordinal id.
    fn from_resource(ordinal: u16, size: Option<PhysicalSize<u32>>) -> Result<Self, BadIcon>;

    /// Create an icon from a resource embedded in this executable or library by its name.
    ///
    /// # Notes
    ///
    /// Due to some internal implementation details of the resource embedding/loading process on
    /// Windows platform, strings that can be interpreted as 16-bit unsigned integers (`"1"`,
    /// `"002"`, etc.) cannot be used as valid resource names, and instead should be passed into
    /// [`from_resource`]:
    ///
    /// [`from_resource`]: IconExtWindows::from_resource
    ///
    /// ```rust,no_run
    /// use aterm_window::platform::windows::IconExtWindows;
    /// use aterm_window::window::Icon;
    ///
    /// assert!(Icon::from_resource_name("app", None).is_ok());
    /// assert!(Icon::from_resource(1, None).is_ok());
    /// assert!(Icon::from_resource(27, None).is_ok());
    /// assert!(Icon::from_resource_name("27", None).is_err());
    /// assert!(Icon::from_resource_name("0027", None).is_err());
    /// ```
    ///
    /// While `0` cannot be used as an ordinal id (see [`from_resource`]), it can be used as a
    /// name:
    ///
    /// [`from_resource`]: IconExtWindows::from_resource
    ///
    /// ```rust,no_run
    /// # use aterm_window::platform::windows::IconExtWindows;
    /// # use aterm_window::window::Icon;
    /// assert!(Icon::from_resource_name("0", None).is_ok());
    /// assert!(Icon::from_resource(0, None).is_err());
    /// ```
    fn from_resource_name(name: &str, size: Option<PhysicalSize<u32>>) -> Result<Self, BadIcon>;
}

impl IconExtWindows for Icon {
    fn from_path<P: AsRef<Path>>(
        path: P,
        size: Option<PhysicalSize<u32>>,
    ) -> Result<Self, BadIcon> {
        let win_icon = crate::platform_impl::WinIcon::from_path(path, size)?;
        Ok(Icon { inner: win_icon })
    }

    fn from_resource(ordinal: u16, size: Option<PhysicalSize<u32>>) -> Result<Self, BadIcon> {
        let win_icon = crate::platform_impl::WinIcon::from_resource(ordinal, size)?;
        Ok(Icon { inner: win_icon })
    }

    fn from_resource_name(name: &str, size: Option<PhysicalSize<u32>>) -> Result<Self, BadIcon> {
        let win_icon = crate::platform_impl::WinIcon::from_resource_name(name, size)?;
        Ok(Icon { inner: win_icon })
    }
}
