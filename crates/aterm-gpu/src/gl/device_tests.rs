// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use super::super::test_device as device;

#[test]
fn a_headless_device_tries_every_drm_device_before_the_rest() {
    // A krunkit guest's Mesa lists the virtio-gpu DRM device and the software device; zink
    // cannot initialize the first, so the second must still be tried.
    let found = ["software-a", "card0", "software-b", "card1"];
    let order = drm_first(&found, |d| d.starts_with("card"));
    assert_eq!(order, ["card0", "card1", "software-a", "software-b"]);
    assert_eq!(
        drm_first(&found, |_| false),
        found,
        "no DRM device: EGL's order"
    );
    assert!(drm_first::<&str>(&[], |_| true).is_empty());
}

#[test]
fn a_refused_device_passes_to_the_next_and_the_error_names_every_refusal() {
    // Venus's DRM device refuses (EGL_NOT_INITIALIZED); the software device after it opens.
    let order = ["card0", "card1", "software"];
    let mut tried = Vec::new();
    let opened = first_that_opens(
        &order,
        |d| {
            tried.push(d);
            Some(if d == "software" {
                Ok(d)
            } else {
                Err(format!("{d}: eglInitialize: EGL_NOT_INITIALIZED"))
            })
        },
        || panic!("the surfaceless display is tried only once every device refused"),
    );
    assert_eq!(opened, Ok("software"));
    assert_eq!(
        tried, order,
        "each device in order, up to the one that opens"
    );

    // A device with no display refuses nothing; every refusal, the fallback's last, is named.
    let refused = first_that_opens(
        &order,
        |d| (d != "card1").then(|| Err::<&str, _>(format!("{d} refused"))),
        || Err("gl: no EGL display".to_owned()),
    );
    assert_eq!(
        refused,
        Err("card0 refused; software refused; gl: no EGL display".to_owned())
    );
    assert_eq!(
        first_that_opens::<&str, _>(&[], |_| None, || Ok("surfaceless")),
        Ok("surfaceless"),
        "no device: the surfaceless display"
    );
}

#[test]
fn the_context_is_current_only_inside_a_guard_and_moves_between_threads() {
    let dev = device();
    let other = device();
    let egl = Egl::get().expect("the device loaded EGL");
    // SAFETY: eglGetProcAddress returns the core entry point (EGL 1.5), whose registry
    // signature is `EGLContext eglGetCurrentContext(void)`.
    let get_current: unsafe extern "C" fn() -> ffi::EGLContext = unsafe {
        std::mem::transmute::<*const c_void, unsafe extern "C" fn() -> ffi::EGLContext>((egl
            .fns
            .get_proc_address)(
            c"eglGetCurrentContext".as_ptr(),
        ))
    };
    // SAFETY: callable at any time.
    let current = || unsafe { get_current() };
    assert!(current().is_null(), "nothing is current before a guard");
    {
        let outer = dev.enter().expect("enter");
        let context = dev
            .0
            .egl_binding()
            .expect("a headless device is EGL's")
            .context;
        assert_eq!(current(), context, "current inside the guard");
        {
            let _inner = dev.enter().expect("a nested enter on the holding thread");
            assert_eq!(current(), context);
        }
        assert_eq!(current(), context, "the nested guard released nothing");
        outer.check("a clean context").expect("no GL error");
        let err = other
            .enter()
            .err()
            .expect("a second device while the first is held");
        assert!(err.contains("another GL device"), "{err}");
        let err = GlDevice::headless().expect_err("creation while a device is held");
        assert!(err.contains("held on this thread"), "{err}");
    }
    assert!(current().is_null(), "the outer guard released the context");
    {
        let _held = other.enter().expect("enter the other device");
        other.0.latch_lost("gl: drilled");
        let err = other
            .enter()
            .err()
            .expect("a nested enter after a latched loss");
        assert!(err.contains("lost (gl: drilled)"), "{err}");
    }
    assert!(current().is_null(), "the refused nested guard kept nothing");
    let moved = dev.clone();
    std::thread::spawn(move || {
        let cur = moved.enter().expect("enter on another thread");
        cur.check("other thread").expect("no GL error");
    })
    .join()
    .expect("the other thread");
    assert!(
        dev.caps().max_texture_dim.get() >= 2048,
        "GLES 3.0 guarantees 2048"
    );
}

#[test]
fn a_lost_context_at_make_current_latches_the_device_lost() {
    let dev = device();
    let control = device();
    assert!(!dev.0.lost(), "a fresh device is live");
    dev.0.inject_context_loss();
    let err = dev
        .enter()
        .err()
        .expect("the drilled bind fails as a lost context's");
    assert!(err.contains("EGL_CONTEXT_LOST"), "{err}");
    assert!(dev.0.lost(), "the failed bind latched the loss");
    let err = dev
        .enter()
        .err()
        .expect("a lost device refuses every guard");
    assert!(err.contains("lost"), "{err}");
    assert!(dev.0.lost(), "a lost device stays lost");
    let cur = control.enter().expect("the undrilled device binds");
    cur.check("the control").expect("no GL error");
    drop(cur);
    assert!(!control.0.lost(), "the latch is per device");
    assert_eq!(control.0.loss_reason(), None);
    assert!(
        dev.0
            .loss_reason()
            .is_some_and(|r| r.contains("EGL_CONTEXT_LOST")),
        "the latch keeps its cause"
    );
}

/// A present's reset check latches the device lost on any reset status
/// but `GL_NO_ERROR`, NVIDIA's purge among them, and names the status as
/// the cause the frontend's downgrade line prints.
#[test]
fn a_reset_status_other_than_no_error_latches_the_device_lost() {
    for status in [
        ffi::GL_GUILTY_CONTEXT_RESET,
        ffi::GL_INNOCENT_CONTEXT_RESET,
        ffi::GL_UNKNOWN_CONTEXT_RESET,
        ffi::GL_PURGED_CONTEXT_RESET_NV,
    ] {
        let name = ffi::gl_reset_name(status);
        let dev = device();
        dev.0.inject_reset(status);
        let cur = dev.enter().expect("enter");
        let err = cur
            .poll_reset("present")
            .expect_err("a reset fails the check");
        drop(cur);
        assert!(
            err.contains(&format!("present: the context was reset ({name})")),
            "{err}"
        );
        assert!(dev.0.lost(), "{name} latched the device lost");
        assert_eq!(dev.0.loss_reason(), Some(err.as_str()));
        let refused = dev
            .enter()
            .err()
            .expect("a lost device refuses every guard");
        assert!(refused.contains(&name), "{refused}");
    }
    let control = device();
    let cur = control.enter().expect("enter");
    cur.poll_reset("present")
        .expect("the driver reports no reset on a live context");
    control.0.inject_reset(ffi::GL_NO_ERROR);
    cur.poll_reset("present").expect("GL_NO_ERROR is no reset");
    drop(cur);
    assert!(!control.0.lost(), "a live device stays live");
    assert_eq!(control.0.loss_reason(), None);
}

/// The reset check's function is read by name, never guessed (glvnd
/// hands out a stub for any name): GLES 3.2's core
/// `glGetGraphicsResetStatus` (NVIDIA's and Mesa's ES contexts on this
/// desk are 3.2), else the spelling of the robustness extension the
/// context lists, `KHR` before `EXT` before `ARB`; none on a 3.0 or 3.1
/// context that lists none.
#[test]
fn the_reset_query_is_gles_3_2_s_or_the_robustness_extension_s() {
    let none = |_: &str| false;
    let only = |ext: &'static str| move |e: &str| e == ext;
    assert_eq!(
        reset_query("OpenGL ES 3.2 NVIDIA 580.159.03", none),
        Some(c"glGetGraphicsResetStatus")
    );
    assert_eq!(
        reset_query("OpenGL ES 3.2 Mesa 25.2.8", none),
        Some(c"glGetGraphicsResetStatus")
    );
    assert_eq!(reset_query("OpenGL ES 3.0 Mesa 25.2.8", none), None);
    assert_eq!(reset_query("OpenGL ES 3.1 v1.r38p1", none), None);
    assert_eq!(
        reset_query("OpenGL ES 3.1 v1.r38p1", only("GL_KHR_robustness")),
        Some(c"glGetGraphicsResetStatusKHR")
    );
    assert_eq!(
        reset_query("OpenGL ES 3.0", only("GL_EXT_robustness")),
        Some(c"glGetGraphicsResetStatusEXT")
    );
    assert_eq!(
        reset_query("OpenGL ES 3.0", only("GL_ARB_robustness")),
        Some(c"glGetGraphicsResetStatusARB")
    );
    assert_eq!(
        reset_query("OpenGL ES 3.0", |e| matches!(
            e,
            "GL_ARB_robustness" | "GL_EXT_robustness" | "GL_KHR_robustness"
        )),
        Some(c"glGetGraphicsResetStatusKHR")
    );
}
