// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A development-only acceptance of the real native Caps setter, retaining UserCapsControl.

#[cfg(all(target_os = "macos", debug_assertions))]
fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let parsed = (|| {
        if args.len() != 3 {
            return None;
        }
        Some((
            args[0].parse::<u32>().ok()?,
            args[1].parse::<u32>().ok()?,
            u64::from_str_radix(args[2].trim_start_matches("0x"), 16).ok()?,
        ))
    })();
    let Some((vendor, product, registry)) = parsed else {
        eprintln!(
            "usage: caps_native_acceptance <decimal-vendor> <decimal-product> <hex-registry>"
        );
        return std::process::ExitCode::from(2);
    };
    match aterm_machines::init_preferences::accept_native_caps_control(vendor, product, registry) {
        Ok(()) => {
            println!(
                "NativeCapsControl verified on {registry:x}; user and unrelated tables unchanged"
            );
            std::process::ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("NativeCapsControl acceptance refused: {why}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(all(target_os = "macos", debug_assertions)))]
fn main() -> std::process::ExitCode {
    eprintln!("This diagnostic requires a macOS development build.");
    std::process::ExitCode::FAILURE
}
