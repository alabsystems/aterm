-------------------------- MODULE PathConfine --------------------------
\* aterm control-socket CONFINEMENT family (the AI's read+write channel is a
\* hard security boundary): the `image`/snapshot write path MUST keep every
\* commit inside its allowed subdirectory. Models the confused-deputy escape
\* fixed in 47cca4b ("control-socket: fix symlink arbitrary-write escape").
\*
\* Executable model of `confine_image_path` + the writer (control_auth.rs):
\*
\*   confine: canon_parent = canonicalize(images_dir)
\*            resolved     = canon_parent.join(file_name)
\*            if !resolved.starts_with(canon_images) { return None }   \* prefix
\*            if symlink_metadata(resolved).is_symlink() { return None } \* FIX
\*            Some(resolved)
\*   write:   std::fs::write(resolved, png_bytes)   \* follows a symlink!
\*
\* The defect: the old guard checked only that the REQUESTED path's parent
\* canonicalized inside images/, then appended the final component verbatim --
\* so a symlink planted AT the last segment (images/shot.png -> /etc/anything)
\* passed the prefix check yet the writer FOLLOWED it and clobbered an arbitrary
\* file OUTSIDE the root. The fix re-checks the RESOLVED real location: a
\* resolved-Outside path (symlink/.. escape) is rejected and never written.
\*
\* We model a single requested write whose final component may be a symlink
\* that re-points the resolved real target Inside or Outside the confinement
\* root. The toggle `linkOutside` is the adversary planting that escape link.
\* A Write step commits to the resolved real location. Properties:
\*
\*   - WriteWithinSubdir: EVERY committed write target is Inside the root --
\*     no arbitrary-location write ever lands (the whole point of confine_*).
\*   - EscapeRejected:    a request that resolves Outside is REJECTED, never
\*     committed (the resolved-Outside path produces no write at all).
\*
\* committed/target are flat scalars (a single bounded request), so ty's
\* flat-state engine represents the whole state directly.

EXTENDS Naturals

CONSTANTS Buggy   \* TRUE: write the REQUESTED path w/o re-checking the resolved
                  \*       real location (symlink escape -> Outside write).
                  \* FALSE: canonicalize + prefix-check the RESOLVED location;
                  \*        a resolved-Outside request is rejected (no write).

VARIABLES
    linkOutside,  \* adversary: does the final-component symlink resolve OUTSIDE
                  \* the root? (modelled as a real location the request maps to)
    decided,      \* has the confine check run for this request yet?
    committed,    \* has a write been committed to disk?
    target        \* the real location written: "none" | "inside" | "outside"

vars == << linkOutside, decided, committed, target >>

Init ==
    /\ linkOutside \in BOOLEAN   \* either an honest request or an escape link
    /\ decided = FALSE
    /\ committed = FALSE
    /\ target = "none"

\* The writer (`std::fs::write` / `write_private`) commits a PNG to the path
\* that `confine_image_path` returned. Whether the commit lands Inside or
\* Outside the root depends ENTIRELY on whether the confine step re-checked the
\* RESOLVED real location -- that is the Buggy branch.
Confine ==
    /\ ~decided
    /\ decided' = TRUE
    /\ IF Buggy
       THEN \* BUG: trust the requested path; the prefix check passes on the
            \* parent but the writer FOLLOWS the symlinked final component, so
            \* the real commit lands wherever the link points (Outside if the
            \* adversary planted an escape).
            /\ committed' = TRUE
            /\ target' = IF linkOutside THEN "outside" ELSE "inside"
       ELSE \* FIX: re-check the RESOLVED real location. A resolved-Outside
            \* request (symlink/.. escape) is rejected -> no write. Only a
            \* resolved-Inside request commits, and only Inside.
            IF linkOutside
            THEN /\ committed' = FALSE   \* rejected: escape never written
                 /\ target' = "none"
            ELSE /\ committed' = TRUE
                 /\ target' = "inside"
    /\ UNCHANGED linkOutside

Next == Confine

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ linkOutside \in BOOLEAN
    /\ decided \in BOOLEAN
    /\ committed \in BOOLEAN
    /\ target \in { "none", "inside", "outside" }

\* The core confinement guarantee: any write that was actually committed
\* landed INSIDE the allowed subdir. No arbitrary-location write ever commits.
WriteWithinSubdir == committed => (target = "inside")

\* A request whose final component resolves OUTSIDE the root must be rejected:
\* it never produces a committed write. (Strengthens the above against the
\* symlink-escape case specifically -- the resolved-Outside path is the bug.)
EscapeRejected == (committed /\ target = "outside") => FALSE
========================================================================
