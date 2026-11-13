// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_full_browser_is_asked_for_headless_and_the_shell_is_not() {
    let profile = Path::new("/p");
    let shell = Browser {
        path: PathBuf::from("/c/chrome-headless-shell"),
        shell: true,
        source: String::new(),
    };
    let full = Browser {
        shell: false,
        ..shell.clone()
    };
    let has = |b: &Browser, a: &str| {
        launch_args(b, profile, Angle::Metal, &[])
            .iter()
            .any(|x| x == a)
    };
    assert!(!has(&shell, "--headless") && has(&full, "--headless"));
    for b in [&shell, &full] {
        assert!(has(b, "--remote-debugging-pipe") && has(b, "--user-data-dir=/p"));
    }
    assert!(is_shell(Path::new("/x/chrome-headless-shell")));
    assert!(!is_shell(Path::new("/x/Google Chrome")));
    assert!(sandbox_off("linux") && !sandbox_off("macos"));
    assert_eq!(
        has(&shell, "--no-sandbox"),
        sandbox_off(std::env::consts::OS)
    );
}

/// Each backend asks for itself, and a session is held to the adapter
/// the pinned shell reports for its backend: a Metal session that fell
/// back to SwiftShader is not a Metal session.
#[test]
fn each_backend_asks_for_itself_and_knows_its_adapter() {
    let shell = Browser {
        path: PathBuf::from("/c/chrome-headless-shell"),
        shell: true,
        source: String::new(),
    };
    let args = |angle| launch_args(&shell, Path::new("/p"), angle, &[]);
    let has = |angle, a: &str| args(angle).iter().any(|x| x == a);
    assert!(has(Angle::Metal, "--use-angle=metal"));
    assert!(!has(Angle::Metal, "--use-angle=swiftshader"));
    assert!(has(Angle::SwiftShader, "--use-angle=swiftshader"));
    let metal = "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Pro, Unspecified Version) (Gl)";
    let swiftshader = "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (LLVM 10.0.0) \
                       (0x0000C0DE)), SwiftShader driver) (Gl)";
    let subzero = "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero) \
                   (0x0000C0DE)), SwiftShader driver) (Gl)";
    assert!(Angle::Metal.drew(metal) && !Angle::Metal.drew(swiftshader));
    assert!(Angle::SwiftShader.drew(swiftshader) && !Angle::SwiftShader.drew(metal));
    assert!(Angle::SwiftShader.drew(subzero) && !Angle::Metal.drew(subzero));
    assert_eq!(angle_backends().last(), Some(&Angle::SwiftShader));
}
