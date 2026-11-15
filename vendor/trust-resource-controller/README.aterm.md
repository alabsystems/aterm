# Maintained native resource sources

The imported base is published Trust `b27fbd29fd181a19d806fa8c8b9c57a0bc160b76`.
Local manifests redirect dependencies to this repository and omit standalone
binaries/examples. Licenses accompany the maintained source. Source bytes and
protocol data create no CPU, memory, execution, SDK or installed Host authority.
Consumer qualification is independent of canonical qualification.

The client member transport preserves Aterm's original 60-second lifecycle
observation. `src/lib.rs` retains its test-only fixture deadline helper. Object
source tests retain the local immutable source fixture, and descriptor transport
tests retain Aterm's owned temporary directory. The probe scheduler's signal
regression runs in a fresh native test process so its process-wide cancellation
state cannot contaminate parallel tests. The isolated signal child also retains
the canonical exclusive signal epoch. These test-only adapters do not change
production behavior. Private broker codec internals retain canonical target
guards; child modules reuse their parents' existing trait imports. The receiver
feature has no helper-spawn requirement;
client/helper APIs retain their explicit Trust std requirements.

Current Aterm main's native-job source-publication and seed-input declarations
remain additive local overlays on that imported base. Their validation is DATA
validation and creates no publication, resource or installation authority.

Generic receiver acceptance is not an independently trusted Host installation
receipt and cannot authorize managed governor bypass or campaign admission.

The native archive client and its exact three source paths are also maintained
from Trust `b8c2c0b648ca9bf7f93a13a1b627817b1cc8c74a`. The client records the
bound operation exchange; its completion is no SDK, Seed or resource-return
authority, and the second Host EOF is not an ACK receipt. All other native
source overlays above remain in place.

`src/validate.rs` `validate_session` is an explicit overlay: a session is aterm's tab ID, `t`
and 10 lower-case Crockford base32 characters, as well as the earlier `s-` and lower-case hex
form the canonical source accepts (aterm LT-01). Its test is `src/tests.rs`
`a_session_is_an_aterm_tab_id_in_either_form`. The controller keeps the canonical source's
wider `s-` rule (1 to 62 hex digits); aterm admits only a whole tab ID (`s-` and exactly 20 hex
digits) at its own wire (`aterm_types::resource_admission`), so every binding that reaches the
controller is one aterm's journal can name.
