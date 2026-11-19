---
name: aterm-resident
description: Maintain machine and session context after aterm assigns this Claude Code session the resident seat. Use for an assigned resident's wake, contextual titles and summaries, incident retrospectives, and addressed fleet delivery.
---
<!-- aterm skill v1 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# Work a resident wake

Use this procedure when aterm has booked your session as the resident. Other
sessions keep their own assignments. Follow the recipe in the resident folder's
`CLAUDE.md` for the current edition and authority.

1. Start with the changed subject and boundary from the wake. Where aterm offers
   `aterm resident due`, read that index first. Otherwise read the subject's
   existing claims with `aterm ctl @<sid> described` and consult its status or
   a bounded part of its log. Use `aterm ctl help log` for the installed bounds.
   Carry the full session ID through a restart or a title change. If an event
   describes a particular run, keep that run's identity with the evidence.
2. For a resource question, identify the machine, sample time, unit and owning
   process or session. Follow the source to establish a fault. Missing samples,
   stderr output, silence and a request for input each describe different things.
   Read the registered object's own definition and freshness before interpreting
   its value. Keep an unavailable reading explicit.
3. For a title, name the work so another person can recognize it. For a summary,
   answer what the session is about, why it matters, what it is doing and what
   changed. Cite facts and their boundary. Use an available model such as Luna
   when appropriate. Attribute the provider and model that supplied the
   interpretation, and retain uncertainty where the evidence runs out.
4. Before the typed store is available, publish only changed description claims
   in the session log. For example, `aterm ctl @<sid> mark - desc field=title value=Cache%20restore`
   sets a title claim. The other slots are `matters` and `description`.
   Percent-encode the value and respect the log's encoded length and rate bounds.
   Re-read `described` to see which claim stands. Give history or a longer reading
   in your own conversation when asked. With the typed store, use its published
   write contract for read facts, summaries and judgements, citing the read
   boundary and the private brief's recipe. Measured records belong to aterm.
5. For fleet traffic, consult `aterm help fabric` for the installed addressed
   transport. Deliver only within the grant and preserve original sender,
   destination and request ID. Record a receipt only when the transport supplies
   it. `queued=1`, `no-bridge=1`, and `ERR timeout id=<n>` leave the message
   pending in the outbox. A delivery receipt and the destination's answer are
   separate events.
   A reply you author yourself uses the resident as sender. For example, for a
   local addressed question use
   `aterm ctl @self post to=@<sid> kind=answer re=<off> -- '<answer>'`.
   Use the request's own reply address and offset. To forward someone else's
   message, use only a transport that retains their original envelope.
   If attribution or delivery is unavailable, record that and stop that delivery.
   A worker's terminal input is outside this route.
6. Investigate a suspected human dependency within the host's existing grants.
   Cite the failed operation, its time and source, each eligible automatic
   recovery and its observed outcome. For subscription exhaustion, check the
   account manager's rotation result. A successful switch belongs in history.
   If the evidence is incomplete, leave the dependency unconfirmed. When only a
   person could advance the operation, identify the missing mechanism, grant
   or action and record the error immediately. A person offering optional feedback is a normal event.
7. Write the retrospective as the booked resident. Identify its computer, the
   booked session ID and the affected session or operation separately. Keep the
   evidence and its read boundary with the record. Use the incident record
   where one is available to you, or your own conversation otherwise. Name the
   component in aterm or aos, the behavior to change, and a test that reproduces
   the original conditions unattended and checks the intended outcome. For a
   missing account-reset wake, exercise a restart across the reset and verify
   that the saved request resumes exactly once with no operator input. Where
   consent must remain a person's, test that the system honors it and continues
   unaffected work. Within your grants, carry the change through the responsible
   project's normal work and verification process. Record the change reference,
   the test actually run and its result. Keep prevention open across wakes while
   verification is pending or fails. An account restored by a person records
   recovery alone.

Read a whole transcript or survey only when a person explicitly asks for that
scope. A routine wake ends when the changed subjects have been handled.
