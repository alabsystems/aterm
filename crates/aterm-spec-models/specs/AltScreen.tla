--------------------------- MODULE AltScreen ---------------------------
\* aterm terminal-emulator CORRECTNESS family: the DEC private mode 1049
\* alternate-screen round-trip (035f10e).
\*
\* Executable model of aterm-term's alt-screen handling (the CSI ?1049 h/l
\* path). No defect was found here this session, but the alt-screen save /
\* restore is a classic high-impact corruption path -- an aliasing or a
\* botched cursor-restore silently scribbles the user's real screen. This
\* spec PROVES the round-trip is lossless, guarding against a future regression.
\*
\*   1049h (enter): SAVE the cursor, switch the active buffer to ALT, and
\*                  CLEAR alt to blanks.  (alt is a fresh, isolated buffer.)
\*   scribble:      mutate ONLY the active buffer's cells + the cursor.
\*   1049l (leave): switch the active buffer back to MAIN and RESTORE the
\*                  cursor to the saved value.
\*
\* The two screens are functions [1..Cells -> 0..MaxVal] (cell value; 0 = blank),
\* NOT growing sets, so ty's flat-state engine represents them directly. We
\* keep a ghost `mainSaved` -- a copy of main captured the first time we ever
\* entered alt -- so the invariant can assert main was untouched across the
\* whole round-trip.
\*
\* THE property (MainRestoredAfterRoundTrip): whenever the active buffer is
\* MAIN, every main cell still equals what it was before we entered alt AND the
\* cursor equals the saved cursor. If alt aliased main (scribble hit main) or
\* 1049l failed to restore the cursor, this is violated.
\*
\* The CONSTANT Buggy selects the defect: Buggy=TRUE makes alt ALIAS main (the
\* scribble mutates the main buffer) and 1049l forget to restore the cursor;
\* Buggy=FALSE is the correct, isolated, cursor-restoring implementation.
\*
\* SHAPE MATTERS (ty constraints, per Snapshot.tla): invariants use bounded
\* integer quantifiers only, and no action assigns a scalar via
\* `x' = IF..THEN..ELSE`; we branch the whole action with formula-level IF.

EXTENDS Naturals

CONSTANTS Cells, MaxVal, Buggy

VARIABLES
    active,     \* "main" or "alt": which buffer is currently displayed
    mainCell,   \* the MAIN screen cells (the user's real screen)
    altCell,    \* the ALT screen cells (scratch buffer)
    cursor,     \* the current cursor position (0..Cells)
    savedCursor,\* the cursor saved by 1049h, restored by 1049l
    entered,    \* have we ever entered alt at least once?
    mainSaved   \* ghost: copy of main captured at the FIRST entry (for the invariant)

vars == << active, mainCell, altCell, cursor, savedCursor, entered, mainSaved >>

Blank == [ c \in 1..Cells |-> 0 ]

Init ==
    /\ active = "main"
    /\ mainCell = Blank
    /\ altCell = Blank
    /\ cursor = 0
    /\ savedCursor = 0
    /\ entered = FALSE
    /\ mainSaved = Blank

\* While on the main screen, the app may write its real content. This is the
\* "before" picture the round-trip must preserve. Only legal when active=main
\* and we have not yet entered alt (after that, main is frozen under the test).
WriteMain(c, v) ==
    /\ active = "main"
    /\ ~entered
    /\ mainCell' = [ mainCell EXCEPT ![c] = v ]
    /\ cursor' = c
    /\ UNCHANGED << active, altCell, savedCursor, entered, mainSaved >>

\* CSI ?1049 h : save cursor, switch to alt, clear alt to blanks. The FIRST
\* time we enter we also snapshot main into the ghost mainSaved.
Enter ==
    /\ active = "main"
    /\ savedCursor' = cursor
    /\ active' = "alt"
    /\ altCell' = Blank
    /\ IF entered
       THEN mainSaved' = mainSaved
       ELSE mainSaved' = mainCell
    /\ entered' = TRUE
    /\ UNCHANGED << mainCell, cursor >>

\* While on the alt screen, the app scribbles. CORRECT (Buggy=FALSE): the edit
\* lands in the isolated alt buffer. BUGGY (Buggy=TRUE): alt ALIASES main, so
\* the scribble corrupts the user's real screen.
Scribble(c, v) ==
    /\ active = "alt"
    /\ cursor' = c
    /\ IF Buggy
       THEN /\ mainCell' = [ mainCell EXCEPT ![c] = v ]
            /\ altCell' = altCell
       ELSE /\ altCell' = [ altCell EXCEPT ![c] = v ]
            /\ mainCell' = mainCell
    /\ UNCHANGED << active, savedCursor, entered, mainSaved >>

\* CSI ?1049 l : switch back to main and restore the cursor. CORRECT
\* (Buggy=FALSE): cursor = savedCursor. BUGGY (Buggy=TRUE): the restore is
\* dropped, leaving the cursor wherever the alt scribble parked it.
Leave ==
    /\ active = "alt"
    /\ active' = "main"
    /\ IF Buggy
       THEN cursor' = cursor
       ELSE cursor' = savedCursor
    /\ UNCHANGED << mainCell, altCell, savedCursor, entered, mainSaved >>

Next ==
    \/ \E c \in 1..Cells, v \in 1..MaxVal : WriteMain(c, v)
    \/ Enter
    \/ \E c \in 1..Cells, v \in 1..MaxVal : Scribble(c, v)
    \/ Leave

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ active \in { "main", "alt" }
    /\ mainCell \in [ 1..Cells -> 0..MaxVal ]
    /\ altCell \in [ 1..Cells -> 0..MaxVal ]
    /\ cursor \in 0..Cells
    /\ savedCursor \in 0..Cells
    /\ entered \in BOOLEAN
    /\ mainSaved \in [ 1..Cells -> 0..MaxVal ]

\* THE round-trip property. Whenever the active buffer is MAIN and we have been
\* through alt at least once, every main cell equals the value it held before
\* we first entered alt (alt edits never aliased main) AND the cursor equals the
\* saved cursor (1049l restored it). Stated only while displaying main: that is
\* exactly the surface the user sees after the round-trip.
MainRestoredAfterRoundTrip ==
    (active = "main" /\ entered) =>
        /\ \A c \in 1..Cells : mainCell[c] = mainSaved[c]
        /\ cursor = savedCursor

\* The cursor never leaves the screen bounds.
CursorBounded == cursor \in 0..Cells /\ savedCursor \in 0..Cells
========================================================================
