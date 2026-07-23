-------------------------- MODULE PresentCoalescing --------------------------
\* Proof-Carrying Performance: the input-to-photon latency fix (v0.5.2).
\*
\* MODELS the present-coalescing soft cap in crates/aterm-gui/src/main.rs
\* (`Wake::Output`) + the frame-pacing stamp in crates/aterm-gui/src/app_render.rs.
\* A typed keystroke's echo is a CONTENT present; the default-on cursor aurora
\* re-presents for ~260 ms after every cursor move (an ANIMATION present that repaints
\* the WHOLE grid — including any pending echo — but is NOT new content).
\*
\* THE SOFT CAP defers a content present that lands within one frame interval of the
\* last present, measured from `last_present_at`. THE BUG (old code, Buggy = 1):
\* `last_present_at` was stamped by EVERY present, including an aurora animation tick.
\* So an echo typed during the aurora tail saw a "recent present" and was DEFERRED — not
\* indefinitely (the deferred-flush still fired it within ONE frame interval), but it
\* lost up to one frame interval (~8 ms on a 120 Hz ProMotion panel — the M-series
\* default; ~16 ms on 60 Hz) of input-to-photon time it should not have. THE FIX
\* (Buggy = 0): only a genuine CONTENT present stamps the cap (`content_pending`), so an
\* aurora tick never defers an echo.
\*
\* Two clocks make the distinction precise:
\*   lastContent — stamped by CONTENT presents only (the fix's `last_present_at`).
\*   lastAny     — stamped by ALL presents incl. aurora (the old `last_present_at`).
\* The cap reads `lastContent` at Buggy = 0 and `lastAny` at Buggy = 1.
\*
\* THE EFFICIENCY THEOREM (`ContentIdleEchoImmediate`): an echo that ARRIVES when the
\* last genuine CONTENT present is already a full frame interval old (`arrivedIdle` — no
\* real output burst is being coalesced) is presented at its arrival instant, never
\* deferred by an intervening aurora animation present. (Legitimate sub-frame coalescing
\* of an ACTUAL content burst — `arrivedIdle = FALSE` — is still allowed and is NOT
\* claimed away.) Proven by exhaustive BFS at Buggy = 0; a counterexample (a
\* content-idle-arriving echo deferred because an aurora present kept the all-presents
\* clock fresh) is caught at Buggy = 1 (the Buggy-constant prove-and-catch convention).
\*
\* Honest scope: a per-window, single-outstanding-echo model. The bound is on time
\* DIFFERENCES (now - lastContent, now - echoArrival), so it is shift-invariant and the
\* small `MaxTime` horizon is adequate; the check is exhaustive over that bounded set.

EXTENDS Naturals

CONSTANTS
  Buggy,         \* 0 = fixed (cap reads lastContent); 1 = old (cap reads lastAny)
  FrameInterval, \* the soft-cap window (MIN_FRAME_INTERVAL), in abstract time units
  MaxTime        \* BFS time horizon (keeps the reachable-state set finite)

VARIABLES
  now,          \* current discrete time
  lastContent,  \* time of the last CONTENT present (the fix's frame-pacing clock)
  lastAny,      \* time of the last present incl. aurora (the old frame-pacing clock)
  everContent,  \* has any content present happened? (FALSE = clock is None)
  everAny,      \* has any present happened?
  echoPending,  \* is an unpresented content echo (a typed char) waiting?
  echoArrival,  \* time the pending echo arrived (meaningful iff echoPending)
  arrivedIdle,  \* did the pending echo ARRIVE while the content clock was already idle?
  auroraActive  \* is the cursor aurora animating? (its ticks present + stamp lastAny)

vars == << now, lastContent, lastAny, everContent, everAny,
           echoPending, echoArrival, arrivedIdle, auroraActive >>

\* The cap reads the FIX's clock (content only) or the OLD clock (all presents).
Clock == IF Buggy = 1 THEN lastAny ELSE lastContent
Ever  == IF Buggy = 1 THEN everAny ELSE everContent
CapAllows == (~Ever) \/ (now - Clock >= FrameInterval)

\* The last genuine CONTENT present is already a full frame interval old: no real output
\* burst is in flight to coalesce.
ContentIdle == everContent /\ (now - lastContent >= FrameInterval)

Init ==
  /\ now = 0
  /\ lastContent = 0
  /\ lastAny = 0
  /\ everContent = FALSE
  /\ everAny = FALSE
  /\ echoPending = FALSE
  /\ echoArrival = 0
  /\ arrivedIdle = FALSE
  /\ auroraActive = FALSE

\* A keystroke arrives: its echo becomes pending and (re)arms the cursor aurora. Record
\* whether it arrived during a content-idle gap (the case the fix must keep immediate).
Type ==
  /\ ~echoPending
  /\ echoPending' = TRUE
  /\ echoArrival' = now
  /\ arrivedIdle' = ContentIdle
  /\ auroraActive' = TRUE
  /\ UNCHANGED << now, lastContent, lastAny, everContent, everAny >>

\* The echo is presented once the cap allows. A CONTENT present stamps BOTH clocks.
ContentPresent ==
  /\ echoPending
  /\ CapAllows
  /\ echoPending' = FALSE
  /\ echoArrival' = 0
  /\ arrivedIdle' = FALSE
  /\ lastContent' = now
  /\ lastAny' = now
  /\ everContent' = TRUE
  /\ everAny' = TRUE
  /\ UNCHANGED << now, auroraActive >>

\* An aurora animation tick PRESENTS a frame: it repaints the whole grid (so it SHOWS a
\* pending echo) and stamps `lastAny` — but it is NOT content, so it never stamps
\* `lastContent`. In the OLD code this `lastAny` stamp is exactly the cap poisoning.
AuroraPresent ==
  /\ auroraActive
  /\ lastAny' = now
  /\ everAny' = TRUE
  /\ echoPending' = FALSE
  /\ echoArrival' = 0
  /\ arrivedIdle' = FALSE
  /\ UNCHANGED << now, lastContent, everContent, auroraActive >>

\* The frame-cap boundary flush: a deferred echo is presented one frame interval after
\* the cap clock — the bound that kept the OLD latency FINITE (one frame). A content
\* present, so it stamps both clocks.
DeferredFlush ==
  /\ echoPending
  /\ Ever
  /\ now - Clock >= FrameInterval
  /\ echoPending' = FALSE
  /\ echoArrival' = 0
  /\ arrivedIdle' = FALSE
  /\ lastContent' = now
  /\ lastAny' = now
  /\ everContent' = TRUE
  /\ everAny' = TRUE
  /\ UNCHANGED << now, auroraActive >>

\* Time advances. It may NOT advance while an echo is due to present THIS instant (the
\* cap allows it): the event loop requests that redraw and presents before the clock
\* moves on. This is what makes the fix's content-idle echo immediate.
Tick ==
  /\ now < MaxTime
  /\ ~(echoPending /\ CapAllows)
  /\ now' = now + 1
  /\ UNCHANGED << lastContent, lastAny, everContent, everAny,
                  echoPending, echoArrival, arrivedIdle, auroraActive >>

\* The aurora decays to nothing (the ~260 ms tail ends).
AuroraDecay ==
  /\ auroraActive
  /\ auroraActive' = FALSE
  /\ UNCHANGED << now, lastContent, lastAny, everContent, everAny,
                  echoPending, echoArrival, arrivedIdle >>

Next ==
  \/ Type
  \/ ContentPresent
  \/ AuroraPresent
  \/ DeferredFlush
  \/ Tick
  \/ AuroraDecay

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ now \in 0..MaxTime
  /\ lastContent \in 0..MaxTime
  /\ lastAny \in 0..MaxTime
  /\ everContent \in BOOLEAN
  /\ everAny \in BOOLEAN
  /\ echoPending \in BOOLEAN
  /\ echoArrival \in 0..MaxTime
  /\ arrivedIdle \in BOOLEAN
  /\ auroraActive \in BOOLEAN

\* THE EFFICIENCY THEOREM: an echo that ARRIVED while the content clock was idle (no
\* output burst to coalesce) is never left pending past its arrival instant — it presents
\* immediately, not deferred by an aurora animation tick. Holds at Buggy = 0 (the cap
\* reads the content clock); violated at Buggy = 1 (the cap reads the all-presents clock,
\* so a recent aurora present defers the content-idle echo by up to one frame interval).
ContentIdleEchoImmediate ==
  (echoPending /\ arrivedIdle) => (now = echoArrival)
=============================================================================
