----------------------------- MODULE GpuEncode -----------------------------
\* aterm GPU-renderer FRAME-ENCODE safety (aterm-gpu/src/renderer.rs ~999): the
\* per-frame background-instance buffer's create/bind/slice discipline.
\*
\* Executable model of `Renderer::render`'s bg-instance path. The CPU walk over
\* cells appends one BgInstance per cell that has a NON-DEFAULT background
\* (Append). The frame is then encoded (Encode): a vertex buffer is created from
\* `bg_inst`, bound, and `bg_buf.slice(..)` is called to draw the fills.
\*
\* The real defect (fixed in 4ab4eb9): the bg buffer was created UNCONDITIONALLY
\* and `bg_buf.slice(..)` called every frame. On a degenerate / zero-cell frame
\* (no cell carried a non-default bg) `bg_inst` was empty, the buffer was
\* zero-size, and wgpu panicked: "buffer slices can not be empty". The fix gates
\* creation+bind on `(!bg_inst.is_empty()).then(|| ...)` + `if let Some(bg_buf)`,
\* so the pass still CLEARS the target but slices nothing when there is no fill.
\*
\* Model it as a slot count `bgInst` in 0..N that accumulates as fills are
\* appended, plus a latched `sliced` flag set the first time a slice/bind occurs.
\* Properties worth proving:
\*
\*   - NeverSliceEmpty: a slice/bind step happens ONLY when bgInst > 0 -- the
\*     exact precondition wgpu requires (a non-empty buffer). This is the
\*     invariant the panic violated.
\*   - SliceImpliesFill: if a slice ever occurred, at least one fill existed --
\*     the contrapositive (an empty frame never slices) is precisely the fix.
\*
\* `sliced` is a BOOLEAN flag and `bgInst` a bounded Natural, so ty's flat-state
\* engine represents the whole state directly.

EXTENDS Naturals

CONSTANTS MaxCells, Buggy

VARIABLES
    bgInst,     \* count of appended BgInstance fills (cells with non-default bg)
    encoded,    \* has this frame been encoded (the render pass run)?
    sliced      \* latched: did an encode ever call bg_buf.slice(..) / bind it?

vars == << bgInst, encoded, sliced >>

Init ==
    /\ bgInst = 0
    /\ encoded = FALSE
    /\ sliced = FALSE

\* CPU cell walk: a cell with a non-default background pushes one BgInstance.
\* Fills only accrue before the frame is encoded.
Append ==
    /\ ~encoded
    /\ bgInst < MaxCells
    /\ bgInst' = bgInst + 1
    /\ UNCHANGED << encoded, sliced >>

\* Encode the frame: create the bg vertex buffer, bind it, draw the fills.
\* Buggy=TRUE  -> the OLD code: slice/bind UNCONDITIONALLY (the zero-cell frame
\*                reaches a slice with bgInst = 0 -> empty-buffer panic).
\* Buggy=FALSE -> the FIX: `(!bg_inst.is_empty()).then(|| ...)` + `if let Some`
\*                only slices/binds when there is at least one fill.
Encode ==
    /\ ~encoded
    /\ encoded' = TRUE
    /\ IF Buggy
       THEN sliced' = TRUE
       ELSE sliced' = (bgInst > 0)
    /\ UNCHANGED bgInst

Next == Append \/ Encode

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ bgInst \in 0..MaxCells
    /\ encoded \in BOOLEAN
    /\ sliced \in BOOLEAN

\* The buffer is sliced/bound ONLY when it holds at least one instance: this is
\* the precise precondition wgpu enforces and the panic violated.
NeverSliceEmpty == sliced => bgInst > 0

\* Contrapositive framing of the fix: a slice ever occurring implies a fill
\* existed -- a zero-cell (all-default-bg) frame never slices an empty buffer.
SliceImpliesFill == sliced => bgInst # 0
========================================================================
