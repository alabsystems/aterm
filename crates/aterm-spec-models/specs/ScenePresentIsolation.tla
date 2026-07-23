-------------------------- MODULE ScenePresentIsolation --------------------------
\* Proof-Carrying Performance: the terminal is ISOLATED from the decorative Scenes layer.
\*
\* MODELS the invariant the v0.5.9 typing-lag regression violated: a decorative subsystem (the
\* animated "Scenes" HUD) must never inflate the terminal's output->present latency, no matter
\* how heavy or buggy it is. Two clocks of failure the real bug had:
\*
\*   1. The scene build FRAME leaked — `SceneController::tick_fill` cleared its output vecs but
\*      not the persistent `self.frame`, so each present APPENDED ~150 sprites that were never
\*      reset; the buffer grew ~150/present without bound (measured →190500), and re-slicing it
\*      cost more every frame (26ms -> 130ms and climbing).
\*   2. That growing build ran SYNCHRONOUSLY on the terminal's output->present critical path, so
\*      every keystroke echo paid the whole (growing) decorative cost.
\*
\* THE FIX (Buggy = 0): (a) each build starts from a cleared frame — bounded; and (b) the build
\* runs on a WORKER thread, so the amount of decorative cost ON the echo's present path is ZERO
\* (`crates/aterm-gui/src/scene_panel.rs` `render_panels` + `scene_worker`; the live present path
\* only memcpy's the worker's latest bounded buffer). The runnable half of this certificate is
\* `scene_build_is_bounded_and_leak_free` in that file (drives the REAL default stack).
\*
\* Buggy-constant prove-and-catch convention (see PresentCoalescing.tla):
\*   * Buggy = 0 (the fix): both invariants hold under EXHAUSTIVE BFS.
\*   * Buggy = 1 (the old code): a reachable state violates them — the scene buffer grows past
\*     its ceiling AND its cost is on the echo's present path, so latency is unbounded.
\*
\* Honest scope: a single-outstanding-echo model over bounded time/lengths; the claim is on the
\* STRUCTURAL coupling (does decorative cost reach the echo path, and can the buffer grow), which
\* is exactly what regressed — not on wall-clock milliseconds.

EXTENDS Naturals

CONSTANTS
  Buggy,         \* 0 = fixed (scene OFF the present path + bounded); 1 = old (on-path + leaks)
  PresentBase,   \* the terminal's own present cost (cell compose + one vsync acquire)
  Emit,          \* one honest scene frame's emission size
  PerFrameCap,   \* the ceiling the scene buffer must never exceed (MAX_SCENE_QUADS abstraction)
  MaxLen,        \* model bound on the buffer (keeps the BFS state set finite)
  LatencyBound,  \* the terminal's allowed output->present latency (must be scene-independent)
  MaxTime        \* BFS time horizon

VARIABLES
  now,          \* current discrete time
  echoPending,  \* is a typed char's echo (a CONTENT present) waiting?
  echoArrival,  \* time the pending echo arrived
  sceneLen      \* the decorative scene build buffer's length

vars == << now, echoPending, echoArrival, sceneLen >>

Min(a, b) == IF a < b THEN a ELSE b

\* The decorative cost that reaches the echo's present. In the FIX it is ZERO (the worker builds
\* off the terminal thread; the UI copies a finished buffer). In the OLD code the entire growing
\* scene buffer is re-sliced ON the present path.
SceneOnPath == IF Buggy = 1 THEN sceneLen ELSE 0

Init ==
  /\ now = 0
  /\ echoPending = FALSE
  /\ echoArrival = 0
  /\ sceneLen = 0

\* A keystroke arrives: its echo becomes a pending CONTENT present.
Type ==
  /\ ~echoPending
  /\ echoPending' = TRUE
  /\ echoArrival' = now
  /\ UNCHANGED << now, sceneLen >>

\* The decorative layer rebuilds. OLD code (Buggy = 1): it APPENDS to a frame it never clears,
\* so the buffer GROWS with uptime (the leak). FIX (Buggy = 0): each build starts from a cleared
\* frame, so the buffer is exactly one emission.
SceneBuild ==
  /\ sceneLen' = IF Buggy = 1 THEN Min(sceneLen + Emit, MaxLen) ELSE Emit
  /\ UNCHANGED << now, echoPending, echoArrival >>

\* Present the pending echo. Its latency is the terminal's base cost PLUS whatever decorative
\* cost is on the path (ZERO in the fix). Time advances by that cost; guarded so the reachable
\* time set stays finite.
Present ==
  /\ echoPending
  /\ now < MaxTime
  /\ echoPending' = FALSE
  /\ echoArrival' = 0
  /\ now' = now + PresentBase + SceneOnPath
  /\ UNCHANGED sceneLen

Tick ==
  /\ now < MaxTime
  /\ now' = now + 1
  /\ UNCHANGED << echoPending, echoArrival, sceneLen >>

Next == Type \/ SceneBuild \/ Present \/ Tick

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ now \in 0..(MaxTime + PresentBase + MaxLen)
  /\ echoPending \in BOOLEAN
  /\ echoArrival \in 0..MaxTime
  /\ sceneLen \in 0..MaxLen

\* INVARIANT 1 (the leak catcher): the decorative buffer never grows past its ceiling.
SceneFrameBounded == sceneLen <= PerFrameCap

\* INVARIANT 2 (the isolation theorem): a pending echo's present latency is bounded by the
\* terminal's OWN cost — the decorative layer contributes nothing to it. Holds at Buggy = 0
\* (SceneOnPath = 0 ⇒ latency = PresentBase); violated at Buggy = 1 (the growing scene buffer is
\* on the echo's present path, so latency = PresentBase + sceneLen, unbounded).
DecorativeLatencyBounded ==
  echoPending => (PresentBase + SceneOnPath) <= LatencyBound
=============================================================================
