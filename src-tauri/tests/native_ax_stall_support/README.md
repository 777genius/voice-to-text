# REAL AX + SYNTHETIC capture — unqualified until a Mac run

New isolated test only. The parent owns Mac compilation and native execution.
No production modules, dependencies, budgets, or hooks change. No microphone,
provider creation, successful paste, clipboard probe, TCC changes, or user app.
Existing AX trust for the actual test executable and an unlocked GUI session
are required. Fixture must remain frontmost; missing preconditions fail.

After the parent compiles on Mac, run the integration binary **alone**:
`VT_REAL_AX_SYNTHETIC=1 VT_AX_STALL_DIR=/tmp/vt-ax-UNIQUE /absolute/test-binary --ignored --exact real_ax_stalled_owned_target --test-threads=1 --nocapture`
Choose a NEW ASCII direct child of `/tmp`; the supervisor creates it mode 0700.
Requires system Python 3.9+ and clang. Supervisor builds its tiny AppKit bundle
there, launches the executable directly, and authenticates private-pipe nonce.
Never launch the fixture/supervisor manually or run this in a shared test process.

Eight predeclared stopped cycles; cycle eight polls forty validate futures on
one runtime: one live ID, then 39 retired-ID contenders after 15 ms. This avoids
admission refusal retiring the live control before native entry. Queued refusals do
not mean forty native stalls. One singleton and long-lived empty-copy sentinel
prove observable drain. Mach ID/count samples corroborate source ownership;
they cannot exclude every transient thread. Rights and VM arrays are released.

A 5 ms synthetic source invokes the real prepared-service callback. Evidence
requires both callback boundaries and fresh owner-matched meter within the first
public request interval. No PCM or meter values are retained by the observer.
Counters bound memory; service error/provider counters must stay zero. The
source self-stops at 25 s; episode must finish below 30 s. Production budgets
remain unchanged: 50–500 ms attribution window and 500 ms sentinel ceiling are
predeclared measurement/precondition tolerances, NOT revised product deadlines.
Raw AX nominal 100 ms read/error/duration is separate from production timing.

Supervisor alone signals its unreaped direct child, checking PID/UID/start/exe
before every signal. WUNTRACED proves STOP acknowledgement; CHECK reads SSTOP.
Three-second stopped watchdog and 25-second whole protocol deadline cannot pass.
EOF/panic/error/watchdog triggers CONT, stdin graceful exit, bounded TERM/KILL,
and one final reap. Failed identity discovery authorizes no signal and requests
exit only through the private pipe. Whole-supervisor kill/session loss and an
unreapable kernel process remain outside repair guarantees. Driver errors survive
cleanup errors; no timeout cleanup is qualification. Parent must externally bound
the disposable driver if a native framework ignores all messaging timeouts.

This is narrow E35 timeout/E58 direct-manager + prepared-capture evidence, NOT
complete Continue routing/provider gating, microphone ingress, revoked trust,
secure input, unsupported fields, precise production syscall timing/error, or
exact live FIFO accounting. No native execution or Mac compilation occurred on
Linux. Pure protocol/state tests: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s src-tauri/tests/native_ax_stall_support -p test_supervisor.py`.

Review corrections: DONE qualifies only a private-pipe requested exit code 0 with no escalation; pre-existing or abnormal target exit fails. Identity lookup loss triggers only private-pipe EOF and a bounded direct-child reap, never another numeric-PID signal. The Rust driver emits fixed first-failure stage and separate cleanup stage codes; framework error strings and panic payloads stay suppressed. Pure supervisor suite now has ten tests. These changes still require exact Mac compilation, independent review and authorized native execution.
