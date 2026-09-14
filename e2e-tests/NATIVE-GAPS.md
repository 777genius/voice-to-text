# Bounded native coverage: E34/E35/E36/E50/E57

Base verified clean at `524cc72c69dc9080cd80c787c3c201eb4bbd91e4`.
All executed checks were Linux source/unit checks. No provider, microphone,
GUI/native execution, deployment or dependency changes were performed.

| Case | Implemented evidence | Residual limit |
| --- | --- | --- |
| E34 | Production snapshot contracts accept indistinguishable final state and reject a different local anchor. Existing E38 readback-unavailable and new E57 contradictory-readback fixtures exercise observable uncertainty. | Identical concurrent edits cannot establish authorship from final AX state. No detection or automatic refusal is claimed for that case. |
| E35 | Four native matrix fault cases: unsupported field, revoked trust, secure field and expired eligibility budget. Actual capture branches fail closed before text snapshot. Prepared-service callbacks run before/after refusal; token remains retained; failed registration cannot insert. No provider factory call, transcript logging or failure text readback in these cases. | Inputs are injected, not actual TCC revocation or secure controls. Callback checks prove readiness after refusal; existing stalled-AX fixture supplies simultaneous capture/timeout instrumentation. No Mac run or scheduling guarantee is claimed. |
| E36 | Compiled production selector, identity, validation, paste and insertion-entry bodies reject self-targets and reactivate the retained editor while either own mini-window identity is frontmost. A different process with the same bundle refuses before insertion. | Native activation, AX objects and insertion effects are substituted in this Linux contract; actual AppKit focus behavior remains unqualified. |
| E50 | Three unsupported target identities feed production qualification, Config offer and Ready acceptance conditions (including unsolicited server capabilities), then actual store delivery-mode/paste bodies. Ordinary IPC fires once; no continuation insertion or sequence advancement. Qualified positive control takes guarded IPC. | This is source-level composition with mocked IPC/logging, not a websocket, full store or OS insertion run. |
| E57 | New native contradictory-readback case reuses E38 retirement/no-retry/clipboard-receipt assertions after real key posting and AX reads. Source contracts reject composed normalization and equal-length anchor mismatch. | Native fixture remains uncompiled/unrun; injected contradiction does not establish real editor normalization. E57 stays partial at the native qualification level. |

Hooks exist only in debug + `native-window-e2e` builds. Capture faults are
one-shot, target-bound and RAII-cleaned; readback faults additionally match run,
delivery sequence and fault kind. Neither retains document text. Source tests
cover wrong identity, overlap rejection, consumption, drop cleanup and stale
handle isolation. Production changes only introduce these fault inputs; no
confirmed product defect or release behavior change was needed. E37/E38 remain.

Run the focused Linux checks (existing Rust toolchain and Node 22.13+ required):

```sh
RUSTC=/root/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc python3 e2e-tests/helpers/nativeGapContracts.py
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s src-tauri/tests/native_ax_stall_support -p test_supervisor.py
```

Results: 9 Rust source/unit tests, 4 store-routing scenarios and 10 existing
supervisor tests passed. Direct installed rustfmt and `git diff --check` passed.
PATH rustup cannot initialize its read-only home; no installation was attempted.
The source runner extracts unchanged function bodies, substitutes declared
platform/domain ports and strips serialization attributes from trace records.
It cannot replace Cargo integration compilation or macOS qualification. Native
matrix cases remain opt-in/ignored under the existing owned-document harness.
