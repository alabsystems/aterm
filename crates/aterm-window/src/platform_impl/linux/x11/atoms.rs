// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/atoms.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (The atoms are interned
// in one round trip through first-party `aterm-x11`, where upstream used x11rb's
// `atom_manager!`: docs/AUDIT-third-party-2026-10-15.md, LW.7.)

//! Collects every atom used by the platform implementation.

use core::ops::Index;

use aterm_x11::proto::Atom;

macro_rules! atom_manager {
    ($($name:ident $(:$lit:literal)?),*) => {
        /// Indices into the `Atoms` table.
        #[derive(Copy, Clone, Debug)]
        #[allow(non_camel_case_types)]
        pub enum AtomName {
            $($name,)*
        }

        /// Every atom's name, in `AtomName` order: the literal where one is
        /// given, else the identifier.
        const NAMES: &[&[u8]] = &[$(atom_manager!(@name $name $(:$lit)?),)*];
    };
    (@name $name:ident) => { stringify!($name).as_bytes() };
    (@name $name:ident : $lit:literal) => { $lit };
}

atom_manager! {
    // General Use Atoms
    CARD32,
    UTF8_STRING,
    WM_CHANGE_STATE,
    WM_CLIENT_MACHINE,
    WM_DELETE_WINDOW,
    WM_PROTOCOLS,
    WM_STATE,
    XIM_SERVERS,

    // Assorted ICCCM Atoms
    _NET_WM_ICON,
    _NET_WM_MOVERESIZE,
    _NET_WM_NAME,
    _NET_WM_PID,
    _NET_WM_PING,
    _NET_WM_STATE,
    _NET_WM_STATE_ABOVE,
    _NET_WM_STATE_BELOW,
    _NET_WM_STATE_FULLSCREEN,
    _NET_WM_STATE_HIDDEN,
    _NET_WM_STATE_MAXIMIZED_HORZ,
    _NET_WM_STATE_MAXIMIZED_VERT,
    _NET_WM_WINDOW_TYPE,

    // Startup notification (the launchee's side, `startup_notify.rs`).
    _NET_STARTUP_INFO_BEGIN,
    _NET_STARTUP_INFO,
    _NET_STARTUP_ID,

    // The one WM window type a window is given (WC.8: the others left with
    // `with_x11_window_type`, and `_XEMBED` with `with_embed_parent_window`).
    _NET_WM_WINDOW_TYPE_NORMAL,

    // Drag-N-Drop Atoms
    XdndAware,
    XdndEnter,
    XdndLeave,
    XdndDrop,
    XdndPosition,
    XdndStatus,
    XdndActionPrivate,
    XdndSelection,
    XdndFinished,
    XdndTypeList,
    TextUriList: b"text/uri-list",
    None: b"None",

    // Miscellaneous Atoms
    _GTK_THEME_VARIANT,
    _MOTIF_WM_HINTS,
    _NET_ACTIVE_WINDOW,
    _NET_CLIENT_LIST,
    _NET_FRAME_EXTENTS,
    _NET_SUPPORTED,
    _NET_SUPPORTING_WM_CHECK,
    _XSETTINGS_SETTINGS
}

/// The atoms used by `winit`, interned once per connection.
#[derive(Debug)]
pub struct Atoms {
    atoms: Vec<Atom>,
}

impl Atoms {
    /// Intern every atom (`only_if_exists` false, so each exists afterwards),
    /// all requests first and then the replies.
    pub fn new(conn: &aterm_x11::Connection) -> Result<Atoms, aterm_x11::Error> {
        let atoms = conn.intern_atoms(NAMES)?;
        debug_assert_eq!(atoms.len(), NAMES.len());
        Ok(Atoms { atoms })
    }
}

impl Index<AtomName> for Atoms {
    type Output = Atom;

    fn index(&self, index: AtomName) -> &Self::Output {
        &self.atoms[index as usize]
    }
}

pub(crate) use AtomName::*;
// Make sure `None` is still defined.
pub(crate) use core::option::Option::None;

#[cfg(test)]
mod tests {
    use super::*;

    /// The names go to the server as written, the two literal ones included,
    /// and in the order `AtomName` indexes.
    #[test]
    fn every_name_is_interned_in_index_order() {
        assert_eq!(NAMES.len(), _XSETTINGS_SETTINGS as usize + 1);
        assert_eq!(NAMES[CARD32 as usize], b"CARD32");
        assert_eq!(NAMES[TextUriList as usize], b"text/uri-list");
        assert_eq!(NAMES[AtomName::None as usize], b"None");
        assert_eq!(NAMES[_XSETTINGS_SETTINGS as usize], b"_XSETTINGS_SETTINGS");
    }
}
