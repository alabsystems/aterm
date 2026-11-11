// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tray-prim pass on Metal (`shaders/tray_prims.metal`, [`crate::tray_prims`]): its pipeline,
//! built only for a device with framebuffer fetch, the composite law's buffer, and one pass's
//! encoding over a card's Rgba8Unorm texture.

use std::sync::mpsc;

use super::encoder::{CommandBuffer, RenderPassDesc, StoreAction};
use super::ffi::{
    AutoreleasePool, Device, LoadAction, MtlScissorRect, Obj, PixelFormat, PrimitiveType,
};
use super::resources::{MetalResourceDevice, SealedTexture};
use crate::device_layer::DeviceHandle;
use crate::pipeline_table::{ShaderLibrary, TRAY_LAYER, TRAY_PRIMS};
use crate::tray_prims::{TrayLayerUniform, TrayPrim, TrayPrimLaw, TrayPrimsUniform};

/// The tray-prim and tray-layer pipelines of one Metal device, and the law it last uploaded.
pub(crate) struct MetalTrayPrims {
    pso: Obj,
    layer_pso: Obj,
    law: Option<(TrayPrimLaw, Obj)>,
}

/// A tray stack's fold, staged ([`MetalTrayPrims::stage_stack`]): the layer pipeline, the law,
/// and each card's uniform, bottom first. The uniforms are bound as inline bytes
/// (`-setVertexBytes:`), which the command buffer copies, so a present allocates no buffer for
/// them.
pub(crate) struct StagedStack {
    pub(crate) pso: Obj,
    pub(crate) law: Obj,
    pub(crate) uniforms: Vec<crate::tray_prims::TrayLayerUniform>,
}

/// Where a stack's card folds: its top-left in the stack card's px, its size, its opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StackCard {
    pub(crate) origin: (i32, i32),
    pub(crate) size: (u32, u32),
    pub(crate) opacity: f32,
}

/// One pass's buffers: the prims, the uniform, and the law ([`MetalTrayPrims::stage`]).
pub(crate) struct StagedPrims {
    stream: Obj,
    uniform: Obj,
    law: Obj,
    count: usize,
}

/// The two pipeline states a build hands its arm ([`TrayPrimsSlot`]).
pub(crate) struct BuiltPipelines {
    pso: Obj,
    layer_pso: Obj,
}

// SAFETY: two `MTLRenderPipelineState`s, immutable once built, which Metal documents as usable
// from any thread; objc retain/release are thread-safe, and each +1 moves once.
unsafe impl Send for BuiltPipelines {}

/// An arm's `MTLDevice`, moved to the worker that builds its tray prims.
struct WorkerDevice(Device);

// SAFETY: an `MTLDevice` may be used from several threads at once (Metal's threading model:
// devices, libraries and pipeline states are thread-safe), and this holder moves its +1 once.
unsafe impl Send for WorkerDevice {}

impl WorkerDevice {
    /// The device, taken by a method so the worker's closure moves the holder (a field read in
    /// the closure would capture the field alone, which is not `Send`).
    #[inline]
    fn into_inner(self) -> Device {
        self.0
    }
}

/// Compile `tray_prims.metal` for `dev` and build its two pipelines.
fn build_pipelines(dev: &Device) -> Result<BuiltPipelines, String> {
    let lib = super::pipelines::compile_library(dev, ShaderLibrary::TrayPrims)?;
    Ok(BuiltPipelines {
        pso: super::pipelines::build(dev, &lib, &TRAY_PRIMS, PixelFormat::Bgra8Unorm)?,
        layer_pso: super::pipelines::build(dev, &lib, &TRAY_LAYER, PixelFormat::Bgra8Unorm)?,
    })
}

/// Why `dev` draws no tray prims: no framebuffer fetch (an Intel or AMD Mac), which their
/// composite needs, so the CPU raster stays the only one.
fn unsupported(dev: &Device) -> Option<String> {
    (!super::fetch::capable(dev)).then(|| {
        format!(
            "{} has no framebuffer fetch, which the tray prims' composite needs",
            dev.name()
        )
    })
}

/// An arm's tray prims, built off the main thread on first ask: compiling `tray_prims.metal`
/// with fast math off and building its two pipelines is a cold compile no frame waits on. Until
/// the build lands, [`Self::poll`] without `wait` answers `None` and the caller folds the deck's
/// cards on the CPU, as a renderer without the prims always does.
#[derive(Default)]
pub(crate) enum TrayPrimsSlot {
    /// Nobody asked yet.
    #[default]
    Unasked,
    /// A worker is building them.
    Building(mpsc::Receiver<Result<BuiltPipelines, String>>),
    /// Built, or why they cannot be (boxed: the pipelines carry the law's 3 KiB tables).
    Ready(Box<Result<MetalTrayPrims, String>>),
}

impl TrayPrimsSlot {
    /// The tray prims of `mint`'s device, starting their build on a worker at the first ask: the
    /// pipelines, or why they cannot be; `None` while the build is under way, unless `wait`,
    /// which waits for it. A worker that cannot start builds them on this thread.
    pub(crate) fn poll(
        &mut self,
        mint: &MetalResourceDevice,
        wait: bool,
    ) -> Option<&mut Result<MetalTrayPrims, String>> {
        if matches!(self, Self::Unasked) {
            *self = Self::start(mint.device());
        }
        if let Self::Building(rx) = self {
            let landed = if wait {
                Some(rx.recv().unwrap_or_else(|_| Err(Self::LOST.to_owned())))
            } else {
                match rx.try_recv() {
                    Ok(built) => Some(built),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => Some(Err(Self::LOST.to_owned())),
                }
            };
            *self = Self::Ready(Box::new(landed?.map(MetalTrayPrims::of)));
        }
        match self {
            Self::Ready(built) => Some(&mut **built),
            Self::Unasked | Self::Building(_) => None,
        }
    }

    /// Why a build that never answered failed.
    const LOST: &'static str = "the tray prims' build ended without an answer";

    /// The build's start for `dev`: refused at once where the device has no framebuffer fetch.
    fn start(dev: &Device) -> Self {
        if let Some(why) = unsupported(dev) {
            return Self::Ready(Box::new(Err(why)));
        }
        let (to, rx) = mpsc::channel();
        let worker = WorkerDevice(dev.clone_ref());
        let spawned = std::thread::Builder::new()
            .name("aterm-metal-tray-prims".to_owned())
            .spawn(move || {
                let dev = worker.into_inner();
                #[cfg(test)]
                held::wait();
                let _pool = AutoreleasePool::new();
                let _ = to.send(build_pipelines(&dev));
            });
        match spawned {
            Ok(_detached) => Self::Building(rx),
            Err(_) => Self::Ready(Box::new(build_pipelines(dev).map(MetalTrayPrims::of))),
        }
    }
}

impl MetalTrayPrims {
    /// The pipelines a build landed ([`TrayPrimsSlot`]), with no law uploaded yet.
    fn of(built: BuiltPipelines) -> Self {
        Self {
            pso: built.pso,
            layer_pso: built.layer_pso,
            law: None,
        }
    }

    /// The law's buffer, uploaded once until the law changes.
    fn law_buffer(&mut self, mint: &MetalResourceDevice, law: &TrayPrimLaw) -> Result<Obj, String> {
        let layer = DeviceHandle::Metal(mint);
        if self.law.as_ref().is_none_or(|(held, _)| held != law) {
            let buf = layer.try_create_uniform_buffer(
                "aterm-gpu tray prim law",
                size_of::<TrayPrimLaw>() as u64,
            )?;
            // SAFETY: a freshly minted shared buffer of exactly the law's size that no GPU work has
            // touched.
            unsafe { layer.write_buffer_from_zero(&buf, aterm_bits::bytes_of(law)) };
            self.law = Some((*law, buf.into_metal().into_obj()));
        }
        Ok(self
            .law
            .as_ref()
            .map(|(_, b)| b.clone_retained())
            .expect("uploaded above"))
    }

    /// What folding `cards` (bottom first) by `law` into a `size` stack card binds: the law's
    /// buffer, uploaded once until it changes, and each card's uniform.
    pub(crate) fn stage_stack(
        &mut self,
        mint: &MetalResourceDevice,
        law: &TrayPrimLaw,
        cards: &[StackCard],
        size: (u32, u32),
    ) -> Result<StagedStack, String> {
        let law = self.law_buffer(mint, law)?;
        let uniforms = cards
            .iter()
            .map(|card| TrayLayerUniform {
                target: [size.0 as f32, size.1 as f32],
                origin: [card.origin.0, card.origin.1],
                size: [
                    i32::try_from(card.size.0).unwrap_or(i32::MAX),
                    i32::try_from(card.size.1).unwrap_or(i32::MAX),
                ],
                opacity: card.opacity.clamp(0.0, 1.0),
                _pad: 0.0,
            })
            .collect();
        Ok(StagedStack {
            pso: self.layer_pso.clone_retained(),
            law,
            uniforms,
        })
    }

    /// The buffers for one pass of `prims` by `law` over a `size` target whose top-left is device
    /// px `origin`, with `offsets`: fresh shared buffers, so a pass still in flight keeps reading
    /// its own (each command buffer retains what it binds). The law is uploaded once until it
    /// changes.
    pub(crate) fn stage(
        &mut self,
        mint: &MetalResourceDevice,
        prims: &[TrayPrim],
        law: &TrayPrimLaw,
        offsets: &[[f32; 2]],
        size: (u32, u32),
        origin: (i32, i32),
    ) -> Result<StagedPrims, String> {
        let law = self.law_buffer(mint, law)?;
        let layer = DeviceHandle::Metal(mint);
        let uniform = TrayPrimsUniform::new(size, origin, offsets);
        let ubuf = layer.try_create_uniform_buffer(
            "aterm-gpu tray prims uniform",
            size_of::<TrayPrimsUniform>() as u64,
        )?;
        // SAFETY: as the law's buffer above.
        unsafe { layer.write_buffer_from_zero(&ubuf, aterm_bits::bytes_of(&uniform)) };
        let bytes: &[u8] = aterm_bits::cast_slice(prims);
        let sbuf = layer.try_create_instance_buffer(
            "aterm-gpu tray prims stream",
            bytes.len().max(size_of::<TrayPrim>()) as u64,
        )?;
        if !bytes.is_empty() {
            // SAFETY: as the law's buffer above; the buffer holds at least `bytes.len()`.
            unsafe { layer.write_buffer_from_zero(&sbuf, bytes) };
        }
        Ok(StagedPrims {
            stream: sbuf.into_metal().into_obj(),
            uniform: ubuf.into_metal().into_obj(),
            law,
            count: prims.len(),
        })
    }

    /// Encode one pass of `staged` over `target` (an Rgba8Unorm card whose bytes the pass loads),
    /// within `scissor` (`x0, y0, x1, y1` in the target's pixels; its whole extent when `None`).
    pub(crate) fn encode(
        &self,
        cb: &mut CommandBuffer<'_>,
        target: &SealedTexture,
        staged: &StagedPrims,
        scissor: Option<[u32; 4]>,
    ) -> Result<(), String> {
        if staged.count == 0 {
            return Ok(());
        }
        let binds = TRAY_PRIMS.binds;
        let pass = cb.render_pass(&RenderPassDesc {
            target,
            load: LoadAction::Load,
            store: StoreAction::Store,
            viewport: None,
            scissor: scissor.map(|[x0, y0, x1, y1]| MtlScissorRect {
                x: x0 as usize,
                y: y0 as usize,
                width: x1.saturating_sub(x0) as usize,
                height: y1.saturating_sub(y0) as usize,
            }),
        })?;
        pass.set_pipeline(&self.pso);
        pass.set_vertex_buffer(
            &staged.uniform,
            binds
                .vertex_uniform
                .expect("the tray prims bind a vertex uniform") as usize,
        )?;
        pass.set_instance_stream(&staged.stream);
        let &[uniform_slot, law_slot, prims_slot] = binds.fragment_buffers else {
            unreachable!("the tray prims bind three fragment buffers");
        };
        pass.set_fragment_buffer(&staged.uniform, uniform_slot as usize);
        pass.set_fragment_buffer(&staged.law, law_slot as usize);
        pass.set_fragment_buffer(&staged.stream, prims_slot as usize);
        pass.draw_instanced(PrimitiveType::TriangleStrip, 4, staged.count)
    }
}

/// A test's hold on every tray-prim build: a worker waits to build while one is held, so a test
/// can ask while the build is certainly under way.
#[cfg(test)]
#[path = "tray_prims_held_tests.rs"]
pub(crate) mod held;
