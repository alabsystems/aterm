// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `dbus-send` sctk-adwaita's `config.rs` spawns, as the recorder runs
//! it: what the real `dbus-send --print-reply=literal` prints for a
//! `Settings.Read` of `org.gnome.desktop.wm.preferences button-layout`
//! through xdg-desktop-portal-gtk, the value from `RECORDER_BUTTON_LAYOUT`
//! (measured on the lane's Ubuntu 24.04 guest, 2026-10-20: three spaces,
//! `variant`, seven spaces, `variant`, ten spaces, the value, no newline).
//! With the variable unset it answers as `dbus-send` does where no portal
//! runs: nothing on stdout, a failure exit.

fn main() {
    match std::env::var("RECORDER_BUTTON_LAYOUT") {
        Ok(value) => print!("   variant       variant          {value}"),
        Err(_) => {
            eprintln!("Error org.freedesktop.DBus.Error.ServiceUnknown: no portal");
            std::process::exit(1);
        }
    }
}
