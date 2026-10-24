// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The VZ arm of [`Machine`] (`docs/DESIGN-vz-guest-2026-10-23.md` §5): Virtualization.framework,
//! `dlopen`ed at the first `Boot` and never `#[link]`ed, so the library, the lab and every
//! test load nothing of it, driven through `aterm-objc` by selector.
//!
//! Every `VZVirtualMachine` call is made on one private [`SerialQueue`], and the framework
//! answers on it: the completion blocks, the VM's delegate and the listener's delegate each
//! hand the core an [`Event`] through its [`Sink`]. The main thread blocks on the control
//! socket and never runs a run loop; nothing here makes an `NSApplication`, and the
//! configuration has no display, input or audio device (fact 45).

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::Read as _;
use std::os::fd::{IntoRawFd as _, OwnedFd};
use std::path::Path;
use std::sync::{Mutex, OnceLock, mpsc};

use aterm_objc::send::send_id;
use aterm_objc::{
    BlockPtr, Bool, ClassPtr, Id, MainThread, Obj, RcBlock, Sel, SerialQueue, checked_msg, class,
    ns_error_string, ns_string, ns_string_to_rust, objc_try, sel,
};
use aterm_vz::detect::SPAN;
use aterm_vz::machine::{Conn, Event, Machine, Sink};
use aterm_vz::plan::{ATERM_TAG, Limits, Plan, ROSETTA_SOCKET, ShareEntry};
use aterm_vz::seed::{BOOT_TAG, ROSETTA_TAG};

/// The framework's binary, opened by path.
const FRAMEWORK: &CStr = c"/System/Library/Frameworks/Virtualization.framework/Virtualization";

const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;

/// `VZLinuxRosettaAvailabilityInstalled`.
const ROSETTA_INSTALLED: isize = 2;

// SAFETY: libSystem's loader and dyld's image list, with the prototypes `<dlfcn.h>` and
// `<mach-o/dyld.h>` give them.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlerror() -> *mut c_char;
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const c_char;
}

/// The GUI images whose presence the helper reports once the framework is loaded: linking
/// Virtualization loads them (fact 45), and V2 measures that loading them opens no
/// WindowServer connection.
const GUI_IMAGES: [&str; 4] = [
    "/AppKit.framework/",
    "/SkyLight.framework/",
    "/HIToolbox.framework/",
    "/CoreGraphics.framework/",
];

/// Open Virtualization.framework, once per process, and log what the process then holds.
fn load() -> Result<(), String> {
    static LOADED: OnceLock<Result<(), String>> = OnceLock::new();
    LOADED
        .get_or_init(|| {
            let gui = |names: &[String]| -> String {
                let found: Vec<&str> = GUI_IMAGES
                    .iter()
                    .filter(|g| names.iter().any(|n| n.contains(**g)))
                    .map(|g| g.trim_matches('/'))
                    .collect();
                if found.is_empty() {
                    "none".to_string()
                } else {
                    found.join(", ")
                }
            };
            let before = image_names();
            // SAFETY: a NUL-terminated path and a valid mode. The handle is never closed:
            // the framework stays loaded for the helper's life.
            let handle = unsafe { dlopen(FRAMEWORK.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
            if handle.is_null() {
                return Err(format!(
                    "NOT RUN — {}: {}",
                    FRAMEWORK.to_string_lossy(),
                    last_error()
                ));
            }
            if class(c"VZVirtualMachine").is_null() {
                return Err(format!(
                    "NOT RUN — {} loaded without VZVirtualMachine",
                    FRAMEWORK.to_string_lossy()
                ));
            }
            let after = image_names();
            eprintln!(
                "aterm-vz-host: loaded Virtualization.framework: {} dyld images, GUI images {}; \
                 before it {}, GUI images {}",
                after.len(),
                gui(&after),
                before.len(),
                gui(&before)
            );
            Ok(())
        })
        .clone()
}

/// Every image dyld has loaded into this process.
fn image_names() -> Vec<String> {
    // SAFETY: dyld's own count, read without arguments.
    let n = unsafe { _dyld_image_count() };
    (0..n)
        .filter_map(|i| {
            // SAFETY: an index below the count; the name is dyld's NUL-terminated string,
            // or null for an image being unloaded.
            let p = unsafe { _dyld_get_image_name(i) };
            // SAFETY: non-null, NUL-terminated, and copied at once.
            (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
        })
        .collect()
}

fn last_error() -> String {
    // SAFETY: `dlerror` returns a NUL-terminated message owned by the loader, or null.
    let message = unsafe { dlerror() };
    if message.is_null() {
        return "no reason given".to_owned();
    }
    // SAFETY: non-null, NUL-terminated, and read before any other dl call on this thread.
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

/// An object made, messaged and released on the VM's queue, or a value object the framework
/// hands across threads (`VZVirtioSocketConnection`, whose descriptor a [`Conn`] borrows).
struct Held(Obj);

// SAFETY: a `Held` moves between the core's thread and the queue only as an owner: every
// message to it is sent inside `SerialQueue::sync` on the VM's queue, and its release is a
// thread-safe `objc_release`.
unsafe impl Send for Held {}

impl Held {
    const fn id(&self) -> Id {
        self.0.id()
    }

    /// Retain a borrowed reference.
    ///
    /// # Safety
    /// `id` is a live object, or nil.
    unsafe fn retain(id: Id) -> Option<Self> {
        // SAFETY: the caller's.
        unsafe { Obj::retain(id) }.map(Self)
    }
}

/// A borrowed object's address, carried into a queue closure whose owner keeps it alive.
#[derive(Clone, Copy)]
struct Raw(Id);

// SAFETY: only an address; it is messaged on the VM's queue while a `Held` in `Vz` keeps
// the object alive.
unsafe impl Send for Raw {}

impl Raw {
    /// The address (a method, so a closure captures the whole `Raw`, never its `Id` field).
    const fn id(self) -> Id {
        self.0
    }
}

aterm_objc::declare_class! {
    /// The VM's delegate: its two methods post to the core's sink (§5).
    struct VmDelegate: NSObject {
        const NAME: &str = "ATermVzHostVmDelegate";
        type Ivars = Sink;
        protocols: [VZVirtualMachineDelegate];

        @sel(guestDidStopVirtualMachine:)
        fn guest_did_stop(&self, _vm: Id) {
            self.ivars().send(Event::GuestStopped);
        }

        @sel(virtualMachine:didStopWithError:)
        fn did_stop_with_error(&self, _vm: Id, error: Id) {
            // SAFETY: the framework passes a live NSError.
            let words = unsafe { error_text(error) };
            self.ivars().send(Event::StoppedWithError(words));
        }
    }
}

aterm_objc::declare_class! {
    /// The watch listener's delegate: a dial of port 1025 becomes the core's
    /// [`Event::Watch`] (§6). The core takes only a boot's first dial (the agent dials once)
    /// and closes any later one unread.
    struct ListenerDelegate: NSObject {
        const NAME: &str = "ATermVzHostListenerDelegate";
        type Ivars = Sink;
        protocols: [VZVirtioSocketListenerDelegate];

        @sel(listener:shouldAcceptNewConnection:fromSocketDevice:)
        fn should_accept(&self, _listener: Id, connection: Id, _device: Id) -> Bool {
            // SAFETY: the framework passes a live VZVirtioSocketConnection.
            match unsafe { conn_of(connection, aterm_vz_agent::WATCH_PORT) } {
                Ok(conn) => {
                    self.ivars().send(Event::Watch(conn));
                    Bool::YES
                }
                Err(e) => {
                    eprintln!("aterm-vz-host: the agent's watch connection: {e}; refused");
                    Bool::NO
                }
            }
        }
    }
}

/// A connection object as a [`Conn`]: retained, its descriptor borrowed from it.
///
/// # Safety
/// `connection` is a live `VZVirtioSocketConnection`.
unsafe fn conn_of(connection: Id, port: u32) -> Result<Conn, String> {
    // SAFETY: the caller's; `fileDescriptor` is `-(int)` on it (`i16@0:8`).
    unsafe {
        let held = Held::retain(connection).ok_or("a nil connection")?;
        let fd: unsafe extern "C-unwind" fn(Id, Sel) -> i32 = checked_msg();
        let fd = fd(held.id(), sel!(fileDescriptor));
        if fd < 0 {
            return Err(format!("the connection's descriptor is {fd}"));
        }
        // `Conn::new`'s contract: the object, which the `Conn` owns, closes the descriptor
        // only when it is destroyed (fact 31), and nothing else closes it.
        Ok(Conn::new(fd, port, Box::new(held)))
    }
}

/// An `NSError` as the design has it: its domain, code and `localizedDescription`, in
/// `-[NSError description]`'s shape.
///
/// # Safety
/// `err` is a live `NSError`, or nil.
unsafe fn error_text(err: Id) -> String {
    if err.is_null() {
        return "the framework failed with no NSError".into();
    }
    // SAFETY: `domain` is `-(NSString *)` and `code` `-(NSInteger)` on a live NSError.
    unsafe {
        let domain = ns_string_to_rust(send_id(err, sel!(domain)));
        let code: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
        let code = code(err, sel!(code));
        format!(
            "Error Domain={domain} Code={code} \"{}\"",
            ns_error_string(err)
        )
    }
}

/// An `NSException`'s name and reason.
///
/// # Safety
/// `exception` is a live `NSException`.
unsafe fn exception_text(exception: Id) -> String {
    // SAFETY: `name` and `reason` are `-(NSString *)` on a live NSException.
    unsafe {
        format!(
            "{}: {}",
            ns_string_to_rust(send_id(exception, sel!(name))),
            ns_string_to_rust(send_id(exception, sel!(reason)))
        )
    }
}

/// `+[cls alloc]`, +1 and uninitialised.
///
/// # Safety
/// The result is sent exactly one initializer.
unsafe fn alloc(cls: &'static CStr) -> Result<Id, String> {
    let c = class(cls);
    if c.is_null() {
        return Err(format!(
            "Virtualization.framework has no class {}",
            cls.to_string_lossy()
        ));
    }
    // SAFETY: `+alloc` on a registered class.
    let raw = unsafe {
        let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
        alloc(c, sel!(alloc))
    };
    if raw.is_null() {
        return Err(format!("+[{} alloc] returned nil", cls.to_string_lossy()));
    }
    Ok(raw)
}

/// An initializer's +1 result, or the words naming the class whose initializer gave nil.
fn owned(id: Id, cls: &CStr, err: Id) -> Result<Obj, String> {
    // SAFETY: `id` is an initializer's +1 result, or nil.
    unsafe { Obj::from_owned(id) }.ok_or_else(|| {
        let why = if err.is_null() {
            "nil".to_string()
        } else {
            // SAFETY: an initializer's `error:` out-parameter holds a live NSError.
            unsafe { error_text(err) }
        };
        format!("-[{} init…]: {why}", cls.to_string_lossy())
    })
}

/// `[[cls alloc] init]`.
fn new(cls: &'static CStr) -> Result<Obj, String> {
    // SAFETY: `-init` is the designated initializer of every class this is called with
    // (each header's `init`), sent once to the fresh `+alloc`.
    let id = unsafe {
        let raw = alloc(cls)?;
        let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
        init(raw, sel!(init))
    };
    owned(id, cls, Id::NIL)
}

/// `[obj sel value]` for a one-object setter.
///
/// # Safety
/// `obj` is live and `sel` a `-(void)set…:(id)` on it.
unsafe fn set(obj: Id, sel: Sel, value: Id) {
    // SAFETY: the caller's.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
        f(obj, sel, value);
    }
}

/// An autoreleased `NSArray` of `items`.
fn array(items: &[Id]) -> Id {
    // SAFETY: `+arrayWithObjects:count:` reads `count` live objects from the buffer during
    // the call and retains them.
    unsafe {
        let f: unsafe extern "C-unwind" fn(ClassPtr, Sel, *const Id, usize) -> Id = checked_msg();
        f(
            class(c"NSArray"),
            sel!(arrayWithObjects:count:),
            items.as_ptr(),
            items.len(),
        )
    }
}

/// `NSString` of `text`.
fn string(text: &str) -> Result<Obj, String> {
    ns_string(text).ok_or_else(|| format!("no NSString for {text:?}"))
}

/// An autoreleased file `NSURL` of `path`.
fn file_url(path: &Path) -> Result<Id, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))?;
    let s = string(text)?;
    // SAFETY: `+fileURLWithPath:` takes an NSString and returns an autoreleased NSURL.
    let url = unsafe {
        let f: unsafe extern "C-unwind" fn(ClassPtr, Sel, Id) -> Id = checked_msg();
        f(class(c"NSURL"), sel!(fileURLWithPath:), s.id())
    };
    if url.is_null() {
        return Err(format!("no file URL for {}", path.display()));
    }
    Ok(url)
}

/// A read-only `VZSharedDirectory` of `path` (§7: no share is writable).
fn shared_directory(path: &Path) -> Result<Obj, String> {
    let url = file_url(path)?;
    // SAFETY: `-initWithURL:readOnly:` (`@28@0:8@16B24`) on a fresh `+alloc`.
    let id = unsafe {
        let raw = alloc(c"VZSharedDirectory")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool) -> Id = checked_msg();
        init(raw, sel!(initWithURL:readOnly:), url, Bool::YES)
    };
    owned(id, c"VZSharedDirectory", Id::NIL)
}

/// The `aterm` share: a `VZMultipleDirectoryShare` of `entries`, each read-only (§7).
fn multiple_share(entries: &[ShareEntry]) -> Result<Obj, String> {
    let mut keys = Vec::with_capacity(entries.len());
    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        keys.push(string(&entry.name)?);
        values.push(shared_directory(&entry.path)?);
    }
    let key_ids: Vec<Id> = keys.iter().map(Obj::id).collect();
    let value_ids: Vec<Id> = values.iter().map(Obj::id).collect();
    // SAFETY: `+dictionaryWithObjects:forKeys:count:` reads `count` live objects from each
    // buffer during the call; `-initWithDirectories:` takes the dictionary.
    let id = unsafe {
        let dict: unsafe extern "C-unwind" fn(ClassPtr, Sel, *const Id, *const Id, usize) -> Id =
            checked_msg();
        let dict = dict(
            class(c"NSDictionary"),
            sel!(dictionaryWithObjects:forKeys:count:),
            value_ids.as_ptr(),
            key_ids.as_ptr(),
            key_ids.len(),
        );
        let raw = alloc(c"VZMultipleDirectoryShare")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
        init(raw, sel!(initWithDirectories:), dict)
    };
    owned(id, c"VZMultipleDirectoryShare", Id::NIL)
}

/// A virtiofs device tagged `tag` carrying `share`.
fn file_system(tag: &str, share: Id) -> Result<Obj, String> {
    let tag_text = string(tag)?;
    // SAFETY: `-initWithTag:` on a fresh `+alloc`; `setShare:` takes a VZDirectoryShare.
    unsafe {
        let raw = alloc(c"VZVirtioFileSystemDeviceConfiguration")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
        let device = owned(
            init(raw, sel!(initWithTag:), tag_text.id()),
            c"VZVirtioFileSystemDeviceConfiguration",
            Id::NIL,
        )?;
        set(device.id(), sel!(setShare:), share);
        Ok(device)
    }
}

/// Rosetta's share, with its caching option at [`ROSETTA_SOCKET`] (§6).
fn rosetta_share() -> Result<Obj, String> {
    let cls = class(c"VZLinuxRosettaDirectoryShare");
    if cls.is_null() {
        return Err("Virtualization.framework has no VZLinuxRosettaDirectoryShare".into());
    }
    // SAFETY: `+availability` is `+(NSInteger)`; `-initWithError:` and
    // `-initWithPath:error:` take an `NSError **` they fill on nil; `setOptions:` takes a
    // VZLinuxRosettaCachingOptions.
    unsafe {
        let availability: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> isize = checked_msg();
        let availability = availability(cls, sel!(availability));
        if availability != ROSETTA_INSTALLED {
            return Err(format!(
                "NOT RUN — Rosetta for Linux is {} on this Mac \
                 (VZLinuxRosettaDirectoryShare.availability {availability})",
                if availability == 0 {
                    "not supported"
                } else {
                    "not installed"
                }
            ));
        }
        let mut err = Id::NIL;
        let raw = alloc(c"VZLinuxRosettaDirectoryShare")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, *mut Id) -> Id = checked_msg();
        let share = owned(
            init(raw, sel!(initWithError:), &raw mut err),
            c"VZLinuxRosettaDirectoryShare",
            err,
        )?;
        let path = string(ROSETTA_SOCKET)?;
        let mut err = Id::NIL;
        let raw = alloc(c"VZLinuxRosettaUnixSocketCachingOptions")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id, *mut Id) -> Id = checked_msg();
        let options = owned(
            init(raw, sel!(initWithPath:error:), path.id(), &raw mut err),
            c"VZLinuxRosettaUnixSocketCachingOptions",
            err,
        )?;
        set(share.id(), sel!(setOptions:), options.id());
        Ok(share)
    }
}

/// What a started VM holds, every object owned here and released on the queue.
struct Built {
    vm: Held,
    /// The running `VZVirtioSocketDevice`.
    socket: Held,
    /// The running `aterm` `VZVirtioFileSystemDevice`, whose `share` `Share` assigns.
    aterm_fs: Option<Held>,
    /// Kept alive: the VM's `delegate` is weak.
    _delegate: Held,
    /// Kept alive with its weak delegate.
    _listener: Held,
    _listener_delegate: Held,
}

/// Build the configuration for `plan` (§5's table), validate it, make the VM on `queue`, set
/// the watch listener on port 1025 before the start, and start it. Runs on the VM's queue.
fn build_and_start(
    plan: &Plan,
    cpus: u32,
    queue: Raw,
    sink: &Sink,
    console: OwnedFd,
) -> Result<Built, String> {
    let start_sink = sink.clone();
    let started = move |err: Id| {
        if err.is_null() {
            start_sink.send(Event::Started);
        } else {
            // SAFETY: the completion's NSError, live for the call.
            start_sink.send(Event::StartFailed(unsafe { error_text(err) }));
        }
    };
    let config = new(c"VZVirtualMachineConfiguration")?;
    let c = config.id();
    // SAFETY: every send below is the header's prototype on a live object this function
    // made (or the framework returned); each initializer is sent once to a fresh `+alloc`.
    unsafe {
        let set_count: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
        set_count(c, sel!(setCPUCount:), usize::try_from(cpus).unwrap_or(1));
        let set_memory: unsafe extern "C-unwind" fn(Id, Sel, u64) = checked_msg();
        set_memory(c, sel!(setMemorySize:), plan.memory_bytes);

        let platform = new(c"VZGenericPlatformConfiguration")?;
        set(c, sel!(setPlatform:), platform.id());

        let kernel = file_url(&plan.kernel)?;
        let raw = alloc(c"VZLinuxBootLoader")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
        let boot = owned(
            init(raw, sel!(initWithKernelURL:), kernel),
            c"VZLinuxBootLoader",
            Id::NIL,
        )?;
        set(
            boot.id(),
            sel!(setInitialRamdiskURL:),
            file_url(&plan.initrd)?,
        );
        let line = string(&plan.command_line)?;
        set(boot.id(), sel!(setCommandLine:), line.id());
        set(c, sel!(setBootLoader:), boot.id());

        let mut disks = Vec::new();
        for disk in &plan.disks {
            let url = file_url(&disk.path)?;
            let mut err = Id::NIL;
            let raw = alloc(c"VZDiskImageStorageDeviceAttachment")?;
            let init: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool, *mut Id) -> Id = checked_msg();
            let attachment = owned(
                init(
                    raw,
                    sel!(initWithURL:readOnly:error:),
                    url,
                    Bool::from(disk.read_only),
                    &raw mut err,
                ),
                c"VZDiskImageStorageDeviceAttachment",
                err,
            )
            .map_err(|e| format!("{}: {e}", disk.path.display()))?;
            let raw = alloc(c"VZVirtioBlockDeviceConfiguration")?;
            let init: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            disks.push(owned(
                init(raw, sel!(initWithAttachment:), attachment.id()),
                c"VZVirtioBlockDeviceConfiguration",
                Id::NIL,
            )?);
        }
        let ids: Vec<Id> = disks.iter().map(Obj::id).collect();
        set(c, sel!(setStorageDevices:), array(&ids));

        let nat = new(c"VZNATNetworkDeviceAttachment")?;
        let net = new(c"VZVirtioNetworkDeviceConfiguration")?;
        set(net.id(), sel!(setAttachment:), nat.id());
        let mac_text = string(&aterm_vz::plan::mac_text(&plan.mac))?;
        let raw = alloc(c"VZMACAddress")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
        let mac = owned(
            init(raw, sel!(initWithString:), mac_text.id()),
            c"VZMACAddress",
            Id::NIL,
        )?;
        set(net.id(), sel!(setMACAddress:), mac.id());
        set(c, sel!(setNetworkDevices:), array(&[net.id()]));

        let socket = new(c"VZVirtioSocketDeviceConfiguration")?;
        set(c, sel!(setSocketDevices:), array(&[socket.id()]));

        let boot_dir = shared_directory(&plan.boot_share)?;
        let raw = alloc(c"VZSingleDirectoryShare")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
        let single = owned(
            init(raw, sel!(initWithDirectory:), boot_dir.id()),
            c"VZSingleDirectoryShare",
            Id::NIL,
        )?;
        let mut shares = vec![
            file_system(BOOT_TAG, single.id())?,
            file_system(ATERM_TAG, multiple_share(&plan.shares)?.id())?,
        ];
        if plan.rosetta {
            shares.push(file_system(ROSETTA_TAG, rosetta_share()?.id())?);
        }
        let ids: Vec<Id> = shares.iter().map(Obj::id).collect();
        set(c, sel!(setDirectorySharingDevices:), array(&ids));

        // hvc0 is written to a pipe the console thread reads; the guest gets no input.
        let raw = alloc(c"NSFileHandle")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, i32, Bool) -> Id = checked_msg();
        let handle = owned(
            init(
                raw,
                sel!(initWithFileDescriptor:closeOnDealloc:),
                console.into_raw_fd(),
                Bool::YES,
            ),
            c"NSFileHandle",
            Id::NIL,
        )?;
        let raw = alloc(c"VZFileHandleSerialPortAttachment")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Id = checked_msg();
        let attachment = owned(
            init(
                raw,
                sel!(initWithFileHandleForReading:fileHandleForWriting:),
                Id::NIL,
                handle.id(),
            ),
            c"VZFileHandleSerialPortAttachment",
            Id::NIL,
        )?;
        let serial = new(c"VZVirtioConsoleDeviceSerialPortConfiguration")?;
        set(serial.id(), sel!(setAttachment:), attachment.id());
        set(c, sel!(setSerialPorts:), array(&[serial.id()]));

        let entropy = new(c"VZVirtioEntropyDeviceConfiguration")?;
        set(c, sel!(setEntropyDevices:), array(&[entropy.id()]));

        let mut err = Id::NIL;
        let validate: unsafe extern "C-unwind" fn(Id, Sel, *mut Id) -> Bool = checked_msg();
        if !validate(c, sel!(validateWithError:), &raw mut err).as_bool() {
            return Err(error_text(err));
        }

        let raw = alloc(c"VZVirtualMachine")?;
        let init: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Id = checked_msg();
        let vm = Held(owned(
            init(raw, sel!(initWithConfiguration:queue:), c, queue.id()),
            c"VZVirtualMachine",
            Id::NIL,
        )?);

        // SAFETY (the witness): neither delegate class has main-thread affinity; its only
        // ivar is a `Sink`, which is `Send + Sync` and may drop on any thread.
        let mtm = MainThread::new_unchecked();
        let delegate = VmDelegate::alloc_init(mtm, sink.clone())
            .ok_or("the VM's delegate could not be made")?;
        let delegate = Held(Obj::from_owned(delegate.into_raw()).ok_or("a nil delegate")?);
        set(vm.id(), sel!(setDelegate:), delegate.id());

        let devices = send_id(vm.id(), sel!(socketDevices));
        let device = Held::retain(send_id(devices, sel!(firstObject)))
            .ok_or("the VM has no socket device")?;
        let listener = Held(new(c"VZVirtioSocketListener")?);
        let listener_delegate = ListenerDelegate::alloc_init(mtm, sink.clone())
            .ok_or("the listener's delegate could not be made")?;
        let listener_delegate =
            Held(Obj::from_owned(listener_delegate.into_raw()).ok_or("a nil delegate")?);
        set(listener.id(), sel!(setDelegate:), listener_delegate.id());
        let listen: unsafe extern "C-unwind" fn(Id, Sel, Id, u32) = checked_msg();
        listen(
            device.id(),
            sel!(setSocketListener:forPort:),
            listener.id(),
            aterm_vz_agent::WATCH_PORT,
        );

        let mut aterm_fs = None;
        let fs_devices = send_id(vm.id(), sel!(directorySharingDevices));
        let count: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
        for i in 0..count(fs_devices, sel!(count)) {
            let fs = at(fs_devices, sel!(objectAtIndex:), i);
            if ns_string_to_rust(send_id(fs, sel!(tag))) == ATERM_TAG {
                aterm_fs = Held::retain(fs);
            }
        }

        let block = RcBlock::new1(started).ok_or("no completion block")?;
        let start: unsafe extern "C-unwind" fn(Id, Sel, BlockPtr) = checked_msg();
        start(
            vm.id(),
            sel!(startWithCompletionHandler:),
            block.as_block_ptr(),
        );

        Ok(Built {
            vm,
            socket: device,
            aterm_fs,
            _delegate: delegate,
            _listener: listener,
            _listener_delegate: listener_delegate,
        })
    }
}

/// The VZ arm: one VM on one private queue.
pub struct Vz {
    queue: SerialQueue,
    built: Option<Built>,
    events: Option<Sink>,
}

impl Vz {
    /// A machine with its queue made and nothing loaded.
    ///
    /// # Errors
    /// libdispatch made no queue.
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            queue: SerialQueue::new(c"aterm-vz-host VM").ok_or("libdispatch made no queue")?,
            built: None,
            events: None,
        })
    }

    fn vm(&self) -> Result<Raw, String> {
        self.built
            .as_ref()
            .map(|b| Raw(b.vm.id()))
            .ok_or_else(|| "no VM was booted".to_string())
    }
}

impl Machine for Vz {
    fn limits(&mut self) -> Result<Limits, String> {
        load()?;
        // SAFETY: `+isSupported` is `+(BOOL)`, `+maximumAllowedCPUCount` `+(NSUInteger)` and
        // `+maximumAllowedMemorySize` `+(uint64_t)`, all class properties with no state.
        let (supported, max_cpus, max_memory) = unsafe {
            let supported: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Bool = checked_msg();
            let cpus: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> usize = checked_msg();
            let memory: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> u64 = checked_msg();
            let config = class(c"VZVirtualMachineConfiguration");
            (
                supported(class(c"VZVirtualMachine"), sel!(isSupported)).as_bool(),
                cpus(config, sel!(maximumAllowedCPUCount)),
                memory(config, sel!(maximumAllowedMemorySize)),
            )
        };
        if !supported {
            return Err(
                "NOT RUN — VZVirtualMachine.isSupported is false: this Mac cannot run a VM".into(),
            );
        }
        let host_cpus = std::thread::available_parallelism()
            .map_err(|e| format!("this host's logical cores: {e}"))?
            .get();
        let host_memory = aterm_sysprobe::physical_memory_mib()
            .ok_or("this host's memory (hw.memsize) could not be read")?;
        Ok(Limits {
            max_cpus: u32::try_from(max_cpus).unwrap_or(u32::MAX),
            max_memory,
            host_cpus: u32::try_from(host_cpus).unwrap_or(u32::MAX),
            host_memory: host_memory.saturating_mul(1 << 20),
        })
    }

    fn boot(&mut self, plan: &Plan, limits: &Limits, events: Sink) -> Result<(), String> {
        load()?;
        if self.built.is_some() {
            return Err("this helper already booted its VM".into());
        }
        let (mut reader, writer) =
            std::io::pipe().map_err(|e| format!("the console's pipe: {e}"))?;
        let cpus = limits.cpus(plan.cpus);
        let queue = Raw(self.queue.as_id());
        let sink = events.clone();
        let built = self
            .queue
            .sync(move || build_and_start(plan, cpus, queue, &sink, OwnedFd::from(writer)))?;
        self.built = Some(built);
        let console = events.clone();
        std::thread::Builder::new()
            .name("vz-console".into())
            .spawn(move || {
                let mut buf = vec![0u8; 16 << 10];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => return,
                        Ok(n) => {
                            console.send(Event::Console(buf.get(..n).unwrap_or_default().to_vec()))
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            eprintln!("aterm-vz-host: the console's pipe: {e}");
                            return;
                        }
                    }
                }
            })
            .map_err(|e| format!("the console thread: {e}"))?;
        self.events = Some(events);
        Ok(())
    }

    fn connect(&mut self, port: u32) -> Result<Conn, String> {
        let socket = self
            .built
            .as_ref()
            .map(|b| Raw(b.socket.id()))
            .ok_or("no VM was booted")?;
        let (tx, rx) = mpsc::channel::<Result<Conn, String>>();
        self.queue.sync(move || -> Result<(), String> {
            let slot = Mutex::new(Some(tx));
            let answered = move |conn: Id, err: Id| {
                let Some(tx) = slot.lock().ok().and_then(|mut s| s.take()) else {
                    return;
                };
                let got = if conn.is_null() {
                    // SAFETY: the completion's NSError, live for the call.
                    Err(unsafe { error_text(err) })
                } else {
                    // SAFETY: the completion's connection, live for the call.
                    unsafe { conn_of(conn, port) }
                };
                let _ = tx.send(got);
            };
            // SAFETY: the completion is `void (^)(VZVirtioSocketConnection *, NSError *)`;
            // `-connectToPort:completionHandler:` is `(uint32_t, block)` on the running
            // device, which `Vz` keeps alive.
            unsafe {
                let block = RcBlock::new2(answered).ok_or("no completion block")?;
                let f: unsafe extern "C-unwind" fn(Id, Sel, u32, BlockPtr) = checked_msg();
                f(
                    socket.id(),
                    sel!(connectToPort:completionHandler:),
                    port,
                    block.as_block_ptr(),
                );
            }
            Ok(())
        })?;
        match rx.recv_timeout(SPAN) {
            Ok(got) => got,
            Err(_) => Err(format!(
                "connectToPort:{port} did not answer within {} s",
                SPAN.as_secs()
            )),
        }
    }

    fn share(&mut self, entries: &[ShareEntry]) -> Result<(), String> {
        let fs = self
            .built
            .as_ref()
            .and_then(|b| b.aterm_fs.as_ref())
            .map(|h| Raw(h.id()))
            .ok_or("the VM has no aterm share device")?;
        self.queue.sync(move || -> Result<(), String> {
            let share = multiple_share(entries)?;
            let reason = Cell::new(None);
            // SAFETY: `setShare:` on the running VZVirtioFileSystemDevice, which `Vz` keeps
            // alive; an unsuitable share raises, which `objc_try` contains.
            objc_try(
                || unsafe { set(fs.id(), sel!(setShare:), share.id()) },
                // SAFETY: the raised object is a live NSException.
                |exception| reason.set(Some(unsafe { exception_text(exception) })),
            )
            .map_err(|_| {
                format!(
                    "the aterm share was not set: {}",
                    reason.take().unwrap_or_default()
                )
            })
        })
    }

    fn request_stop(&mut self) -> Result<(), String> {
        let vm = self.vm()?;
        self.queue.sync(move || {
            let mut err = Id::NIL;
            // SAFETY: `-requestStopWithError:` is `(NSError **) -> BOOL` on the VM, which
            // `Vz` keeps alive, sent on its queue.
            unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, *mut Id) -> Bool = checked_msg();
                if f(vm.id(), sel!(requestStopWithError:), &raw mut err).as_bool() {
                    Ok(())
                } else {
                    Err(error_text(err))
                }
            }
        })
    }

    fn hard_stop(&mut self) -> Result<(), String> {
        let vm = self.vm()?;
        let sink = self.events.clone().ok_or("no VM was booted")?;
        self.queue.sync(move || -> Result<(), String> {
            let stopped = move |err: Id| {
                let done = if err.is_null() {
                    Ok(())
                } else {
                    // SAFETY: the completion's NSError, live for the call.
                    Err(unsafe { error_text(err) })
                };
                sink.send(Event::HardStopped(done));
            };
            // SAFETY: the completion is `void (^)(NSError *)`; `-stopWithCompletionHandler:`
            // takes it on the VM, which `Vz` keeps alive, sent on its queue.
            unsafe {
                let block = RcBlock::new1(stopped).ok_or("no completion block")?;
                let f: unsafe extern "C-unwind" fn(Id, Sel, BlockPtr) = checked_msg();
                f(
                    vm.id(),
                    sel!(stopWithCompletionHandler:),
                    block.as_block_ptr(),
                );
            }
            Ok(())
        })
    }
}

impl Drop for Vz {
    fn drop(&mut self) {
        if let Some(built) = self.built.take() {
            // Released where it was made and messaged.
            self.queue.sync(move || drop(built));
        }
    }
}
