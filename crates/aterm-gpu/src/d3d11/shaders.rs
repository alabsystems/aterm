// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The HLSL twins (`shaders/<library>.hlsl`), compiled into the binary, and
//! the compiler that turns one entry point of one twin into bytecode.
//!
//! The compiler is `D3DCompile` from `d3dcompiler_47.dll`, the FXC that
//! wgpu's DX12 backend compiles naga's HLSL with when no DXC is installed
//! (its default), loaded from System32 only, with the flags wgpu passes it:
//! `ENABLE_STRICTNESS` always, and `DEBUG | SKIP_OPTIMIZATION` in a build
//! with debug assertions (wgpu's `InstanceFlags::DEBUG`). The arm is held
//! byte-identical to wgpu-dx12's frames, and an optimizer that reorders a
//! float expression on one side only breaks that, so the flags are the
//! oracle's. Shader model 5.0: feature level 11_0's (wgpu asks FXC for 5.1,
//! which D3D11 cannot load; the two compile the same arithmetic).
//!
//! Bytecode is cached per process by (library, entry point, stage): a second
//! window's pipelines are built from the first's compile.

use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use aterm_win32::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use aterm_win32::com::Com;
use aterm_win32::d3d11 as d3d;

use crate::pipeline_table::ShaderLibrary;

/// The HLSL twin of one library.
#[inline]
pub(crate) const fn source(library: ShaderLibrary) -> &'static str {
    match library {
        ShaderLibrary::Cell => include_str!("../../shaders/cell.hlsl"),
        ShaderLibrary::Blit => include_str!("../../shaders/blit.hlsl"),
        ShaderLibrary::HdrGlow => include_str!("../../shaders/hdr_glow.hlsl"),
        ShaderLibrary::Tray => include_str!("../../shaders/tray.hlsl"),
        ShaderLibrary::Bloom => include_str!("../../shaders/bloom.hlsl"),
        ShaderLibrary::Shimmer => include_str!("../../shaders/shimmer.hlsl"),
    }
}

/// Which stage an entry point is compiled for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Stage {
    Vertex,
    Pixel,
}

impl Stage {
    /// The FXC target profile, NUL-terminated.
    #[inline]
    const fn target(self) -> &'static [u8] {
        match self {
            Self::Vertex => b"vs_5_0\0",
            Self::Pixel => b"ps_5_0\0",
        }
    }
}

/// The flags every compile passes (see the module header).
pub(crate) const FLAGS: u32 = if cfg!(debug_assertions) {
    d3d::D3DCOMPILE_ENABLE_STRICTNESS | d3d::D3DCOMPILE_DEBUG | d3d::D3DCOMPILE_SKIP_OPTIMIZATION
} else {
    d3d::D3DCOMPILE_ENABLE_STRICTNESS
};

/// `D3DCompile`'s signature (aterm-win32's `pD3DCompile`, unwrapped).
type CompileFn = unsafe extern "system" fn(
    *const c_void,
    usize,
    aterm_win32::core::PCSTR,
    *const d3d::D3D_SHADER_MACRO,
    *mut c_void,
    aterm_win32::core::PCSTR,
    aterm_win32::core::PCSTR,
    u32,
    u32,
    *mut *mut c_void,
    *mut *mut c_void,
) -> aterm_win32::core::HRESULT;

/// `D3DCompile`, loaded once per process; `Err` says why it is missing.
fn compiler() -> Result<CompileFn, String> {
    static COMPILER: OnceLock<Result<usize, String>> = OnceLock::new();
    let addr = COMPILER.get_or_init(|| {
        let name: Vec<u16> = "d3dcompiler_47.dll\0".encode_utf16().collect();
        // SAFETY: a NUL-terminated wide name; System32 only, so a DLL planted
        // beside the executable or in the working directory is never loaded.
        // The module stays loaded for the process (never freed).
        let module = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(format!(
                "d3d11: d3dcompiler_47.dll is not in System32 ({})",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: a loaded module and a NUL-terminated export name.
        let f = unsafe { GetProcAddress(module, c"D3DCompile".as_ptr().cast()) };
        f.map(|f| f as usize)
            .ok_or_else(|| "d3d11: d3dcompiler_47.dll exports no D3DCompile".to_owned())
    });
    let addr = addr.clone()?;
    // SAFETY: the address of `D3DCompile`, whose signature is `CompileFn`
    // (aterm-win32's `pD3DCompile`, held to windows 0.62.2 by its oracle).
    Ok(unsafe { std::mem::transmute::<usize, CompileFn>(addr) })
}

/// A blob's bytes.
fn blob_bytes(blob: &Com) -> &[u8] {
    // SAFETY: `blob` is an ID3DBlob; its buffer lives as long as the blob.
    unsafe {
        let vt = blob.vtable::<d3d::ID3DBlob_Vtbl>();
        let ptr = (vt.GetBufferPointer)(blob.as_raw());
        let len = (vt.GetBufferSize)(blob.as_raw());
        if ptr.is_null() || len == 0 {
            return &[];
        }
        std::slice::from_raw_parts(ptr.cast::<u8>(), len)
    }
}

/// Compile `src`'s `entry` for `stage`; `Err` carries the compiler's log.
pub(crate) fn compile_source(
    src: &str,
    file: &str,
    entry: &str,
    stage: Stage,
) -> Result<Vec<u8>, String> {
    let compile = compiler()?;
    let file_c = CString::new(file).map_err(|_| format!("d3d11: a NUL in {file:?}"))?;
    let entry_c = CString::new(entry).map_err(|_| format!("d3d11: a NUL in {entry:?}"))?;
    let (mut code, mut errors) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: `src` is `src.len()` bytes; the file, entry and target names are
    // NUL-terminated; no macros and no include handler; the two out-pointers
    // are written with one owned blob each (or null).
    let hr = unsafe {
        compile(
            src.as_ptr().cast(),
            src.len(),
            file_c.as_ptr().cast(),
            std::ptr::null(),
            std::ptr::null_mut(),
            entry_c.as_ptr().cast(),
            stage.target().as_ptr(),
            FLAGS,
            0,
            &mut code,
            &mut errors,
        )
    };
    // SAFETY: each out-pointer is null or one owned blob.
    let (code, errors) = unsafe { (Com::from_raw(code), Com::from_raw(errors)) };
    let log = errors
        .as_ref()
        .map(|e| {
            String::from_utf8_lossy(blob_bytes(e))
                .trim_end_matches('\0')
                .trim()
                .to_owned()
        })
        .unwrap_or_default();
    match code {
        Some(code) if hr >= 0 => Ok(blob_bytes(&code).to_vec()),
        _ => Err(format!(
            "d3d11: {entry} ({}) does not compile ({hr:#010x}):\n{log}",
            String::from_utf8_lossy(&stage.target()[..6])
        )),
    }
}

/// The library's file name in a compiler message.
#[inline]
const fn file_name(library: ShaderLibrary) -> &'static str {
    match library {
        ShaderLibrary::Cell => "cell.hlsl",
        ShaderLibrary::Blit => "blit.hlsl",
        ShaderLibrary::HdrGlow => "hdr_glow.hlsl",
        ShaderLibrary::Tray => "tray.hlsl",
        ShaderLibrary::Bloom => "bloom.hlsl",
        ShaderLibrary::Shimmer => "shimmer.hlsl",
    }
}

/// One cached compile's key.
type Key = (ShaderLibrary, &'static str, Stage);

/// `library`'s `entry` for `stage`, compiled once per process.
pub(crate) fn bytecode(
    library: ShaderLibrary,
    entry: &'static str,
    stage: Stage,
) -> Result<Arc<[u8]>, String> {
    static CACHE: Mutex<Option<HashMap<Key, Arc<[u8]>>>> = Mutex::new(None);
    let key = (library, entry, stage);
    if let Some(hit) = CACHE
        .lock()
        .map_err(|_| "d3d11: the shader cache is poisoned".to_owned())?
        .as_ref()
        .and_then(|m| m.get(&key))
    {
        return Ok(Arc::clone(hit));
    }
    // Compiled outside the lock: two threads may compile one entry point
    // once each, and the second insert keeps the first's bytes.
    let code: Arc<[u8]> = compile_source(source(library), file_name(library), entry, stage)?.into();
    let mut cache = CACHE
        .lock()
        .map_err(|_| "d3d11: the shader cache is poisoned".to_owned())?;
    Ok(Arc::clone(
        cache
            .get_or_insert_with(HashMap::new)
            .entry(key)
            .or_insert(code),
    ))
}
