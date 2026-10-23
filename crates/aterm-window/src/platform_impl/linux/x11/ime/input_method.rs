// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/ime/input_method.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::ffi::{CStr, CString, IntoStringError};
use std::os::raw::{c_char, c_ulong, c_ushort};
use std::sync::{Arc, Mutex};
use std::{env, fmt, ptr};

use super::super::atoms::*;
use super::{ffi, util, XConnection, XError};
use aterm_x11::proto::{atom, xproto};

static GLOBAL_LOCK: Mutex<()> = Mutex::new(());

unsafe fn open_im(xconn: &Arc<XConnection>, locale_modifiers: &CStr) -> Option<ffi::XIM> {
    let _lock = GLOBAL_LOCK.lock();

    unsafe { (xconn.xlib.XSetLocaleModifiers)(locale_modifiers.as_ptr()) };

    let im = unsafe {
        (xconn.xlib.XOpenIM)(xconn.display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut())
    };

    if im.is_null() {
        None
    } else {
        Some(im)
    }
}

#[derive(Debug)]
pub struct InputMethod {
    pub im: ffi::XIM,
    pub preedit_style: Style,
    pub none_style: Style,
    _name: String,
}

impl InputMethod {
    fn new(xconn: &Arc<XConnection>, im: ffi::XIM, name: String) -> Option<Self> {
        let mut styles: *mut XIMStyles = std::ptr::null_mut();

        unsafe {
            if !(xconn.xlib.XGetIMValues)(
                im,
                ffi::xn::QUERY_INPUT_STYLE.as_ptr() as *const _,
                (&mut styles) as *mut _,
                std::ptr::null_mut::<()>(),
            )
            .is_null()
            {
                return None;
            }
        }

        let mut preedit_style = None;
        let mut none_style = None;

        unsafe {
            std::slice::from_raw_parts((*styles).supported_styles, (*styles).count_styles as _)
                .iter()
                .for_each(|style| match *style {
                    XIM_PREEDIT_STYLE => {
                        preedit_style = Some(Style::Preedit(*style));
                    },
                    XIM_NOTHING_STYLE if preedit_style.is_none() => {
                        preedit_style = Some(Style::Nothing(*style))
                    },
                    XIM_NONE_STYLE => none_style = Some(Style::None(*style)),
                    _ => (),
                });

            (xconn.xlib.XFree)(styles.cast());
        };

        if preedit_style.is_none() && none_style.is_none() {
            return None;
        }

        let preedit_style = preedit_style.unwrap_or_else(|| none_style.unwrap());
        let none_style = none_style.unwrap_or(preedit_style);

        Some(InputMethod { im, _name: name, preedit_style, none_style })
    }
}

const XIM_PREEDIT_STYLE: XIMStyle =
    ffi::xim_style::PREEDIT_CALLBACKS | ffi::xim_style::STATUS_NOTHING;
const XIM_NOTHING_STYLE: XIMStyle =
    ffi::xim_style::PREEDIT_NOTHING | ffi::xim_style::STATUS_NOTHING;
const XIM_NONE_STYLE: XIMStyle = ffi::xim_style::PREEDIT_NONE | ffi::xim_style::STATUS_NONE;

/// Style of the IME context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Preedit callbacks.
    Preedit(XIMStyle),

    /// Nothing.
    Nothing(XIMStyle),

    /// No IME.
    None(XIMStyle),
}

impl Default for Style {
    fn default() -> Self {
        Style::None(XIM_NONE_STYLE)
    }
}

#[repr(C)]
#[derive(Debug)]
struct XIMStyles {
    count_styles: c_ushort,
    supported_styles: *const XIMStyle,
}

pub(crate) type XIMStyle = c_ulong;

#[derive(Debug)]
pub enum InputMethodResult {
    /// Input method used locale modifier from `XMODIFIERS` environment variable.
    XModifiers(InputMethod),
    /// Input method used internal fallback locale modifier.
    Fallback(InputMethod),
    /// Input method could not be opened using any locale modifier tried.
    Failure,
}

impl InputMethodResult {
    pub fn is_fallback(&self) -> bool {
        matches!(self, InputMethodResult::Fallback(_))
    }

    pub fn ok(self) -> Option<InputMethod> {
        use self::InputMethodResult::*;
        match self {
            XModifiers(im) | Fallback(im) => Some(im),
            Failure => None,
        }
    }
}

#[derive(Debug, Clone)]
enum GetXimServersError {
    XError(#[allow(dead_code)] XError),
    GetPropertyError(#[allow(dead_code)] util::GetPropertyError),
    InvalidUtf8(#[allow(dead_code)] IntoStringError),
}

impl From<util::GetPropertyError> for GetXimServersError {
    fn from(error: util::GetPropertyError) -> Self {
        GetXimServersError::GetPropertyError(error)
    }
}

unsafe fn get_xim_servers(xconn: &Arc<XConnection>) -> Result<Vec<String>, GetXimServersError> {
    let atoms = xconn.atoms();
    let servers_atom = atoms[XIM_SERVERS];

    let root = unsafe { (xconn.xlib.XDefaultRootWindow)(xconn.display) };

    let mut atoms: Vec<ffi::Atom> = xconn
        .get_property::<xproto::Atom>(root as xproto::Window, servers_atom, atom::ATOM)
        .map_err(GetXimServersError::GetPropertyError)?
        .into_iter()
        .map(|atom| atom as _)
        .collect::<Vec<_>>();

    let mut names: Vec<*const c_char> = Vec::with_capacity(atoms.len());
    unsafe {
        (xconn.xlib.XGetAtomNames)(
            xconn.display,
            atoms.as_mut_ptr(),
            atoms.len() as _,
            names.as_mut_ptr() as _,
        )
    };
    unsafe { names.set_len(atoms.len()) };

    let mut formatted_names = Vec::with_capacity(names.len());
    for name in names {
        let string = unsafe { CStr::from_ptr(name) }
            .to_owned()
            .into_string()
            .map_err(GetXimServersError::InvalidUtf8)?;
        unsafe { (xconn.xlib.XFree)(name as _) };
        formatted_names.push(string.replace("@server=", "@im="));
    }
    xconn.check_errors().map_err(GetXimServersError::XError)?;
    Ok(formatted_names)
}

#[derive(Clone)]
struct InputMethodName {
    c_string: CString,
    string: String,
}

impl InputMethodName {
    pub fn from_string(string: String) -> Self {
        let c_string = CString::new(string.clone())
            .expect("String used to construct CString contained null byte");
        InputMethodName { c_string, string }
    }

    pub fn from_str(string: &str) -> Self {
        let c_string =
            CString::new(string).expect("String used to construct CString contained null byte");
        InputMethodName { c_string, string: string.to_owned() }
    }
}

impl fmt::Debug for InputMethodName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.string.fmt(f)
    }
}

#[derive(Debug, Clone)]
struct PotentialInputMethod {
    name: InputMethodName,
    successful: Option<bool>,
}

impl PotentialInputMethod {
    pub fn from_string(string: String) -> Self {
        PotentialInputMethod { name: InputMethodName::from_string(string), successful: None }
    }

    pub fn from_str(string: &str) -> Self {
        PotentialInputMethod { name: InputMethodName::from_str(string), successful: None }
    }

    pub fn reset(&mut self) {
        self.successful = None;
    }

    pub fn open_im(&mut self, xconn: &Arc<XConnection>) -> Option<InputMethod> {
        let im = unsafe { open_im(xconn, &self.name.c_string) };
        self.successful = Some(im.is_some());
        im.and_then(|im| InputMethod::new(xconn, im, self.name.string.clone()))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PotentialInputMethods {
    xmodifiers: Option<PotentialInputMethod>,
    fallbacks: [PotentialInputMethod; 2],
    _xim_servers: Result<Vec<String>, GetXimServersError>,
}

impl PotentialInputMethods {
    pub fn new(xconn: &Arc<XConnection>) -> Self {
        let xmodifiers = env::var("XMODIFIERS").ok().map(PotentialInputMethod::from_string);
        PotentialInputMethods {
            xmodifiers,
            fallbacks: [
                PotentialInputMethod::from_str("@im=local"),
                PotentialInputMethod::from_str("@im="),
            ],
            _xim_servers: unsafe { get_xim_servers(xconn) },
        }
    }

    fn reset(&mut self) {
        if let Some(ref mut input_method) = self.xmodifiers {
            input_method.reset();
        }

        for input_method in &mut self.fallbacks {
            input_method.reset();
        }
    }

    pub fn open_im(
        &mut self,
        xconn: &Arc<XConnection>,
        callback: Option<&dyn Fn()>,
    ) -> InputMethodResult {
        use self::InputMethodResult::*;

        self.reset();

        if let Some(ref mut input_method) = self.xmodifiers {
            let im = input_method.open_im(xconn);
            if let Some(im) = im {
                return XModifiers(im);
            } else if let Some(ref callback) = callback {
                callback();
            }
        }

        for input_method in &mut self.fallbacks {
            let im = input_method.open_im(xconn);
            if let Some(im) = im {
                return Fallback(im);
            }
        }

        Failure
    }
}
