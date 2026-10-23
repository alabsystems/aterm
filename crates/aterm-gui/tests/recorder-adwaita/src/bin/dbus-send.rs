// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recorder stand-in for `dbus-send` reading `button-layout` from `RECORDER_BUTTON_LAYOUT`.

fn main() {
    match std::env::var("RECORDER_BUTTON_LAYOUT") {
        Ok(value) => print!("   variant       variant          {value}"),
        Err(_) => {
            eprintln!("Error org.freedesktop.DBus.Error.ServiceUnknown: no portal");
            std::process::exit(1);
        }
    }
}
