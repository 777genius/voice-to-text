"""Compile the real TEST trace and insertion boundary without Cargo/platform dependencies.
Only serialization attributes are stripped; execution authorization is a test stub.
"""
from pathlib import Path
import re, subprocess, tempfile
root = Path(__file__).resolve().parents[2]
s = (root / 'src-tauri/src/infrastructure/continuation_context.rs').read_text()
def block(start):
    pos = s.index(start)
    brace = s.index('{', pos)
    depth = 1
    end = brace + 1
    while depth:
        depth += (s[end] == '{') - (s[end] == '}')
        end += 1
    return s[pos:end]
parts = [block('pub(crate) mod observation'), block('pub(crate) trait TextClipboard'), block('pub(crate) fn insert_with_current_clipboard')]
code = '\n'.join(parts)
code = re.sub(r'#\[serde\([^\n]*\)\]\n', '', code).replace(', Serialize', '')
code = '''use std::sync::{Mutex, OnceLock};
#[derive(Clone, Copy, Debug, PartialEq)] enum GuardedPasteOutcome { Confirmed { revision: u64 }, Unavailable, ContextMismatch, Uncertain }
struct Execution { trace: Option<usize> }
thread_local! { static EXECUTION: std::cell::RefCell<Option<Execution>> = const { std::cell::RefCell::new(None) }; }
fn begin_effect() -> bool { true }
''' + code + '''
struct Clipboard(i64);
impl TextClipboard for Clipboard {
 type Snapshot = String;
 fn restore(&mut self, _: &String, _: i64) -> Option<i64> { None }
 fn read(&mut self) -> Option<String> { None }
 fn revision(&mut self) -> Option<i64> { Some(self.0) }
 fn write(&mut self, _: &str, _: i64) -> Option<i64> { None }
}
#[test] fn actual_boundary_refusal_uncertain_confirmed_and_duplicate_noop() {
 let mut clipboard = Clipboard(1);
 for (seq, publication, result) in [(1, 2, GuardedPasteOutcome::ContextMismatch), (2, 1, GuardedPasteOutcome::Uncertain), (3, 1, GuardedPasteOutcome::Confirmed { revision: 1 })] {
  let index = observation::queued(7, seq, false).unwrap();
  EXECUTION.with(|c| *c.borrow_mut() = Some(Execution { trace: Some(index) }));
  let returned = insert_with_current_clipboard(&mut clipboard, publication, || {
   assert!(observation::snapshot().records[index].insertion_start_ms.is_some());
   result
  });
  observation::finish(Some(index), returned);
  let r = observation::snapshot().records[index].clone();
  assert_eq!(returned, result);
  assert_eq!(r.insertion_start_ms.is_some(), publication == 1);
  assert_eq!(r.insertion_confirmed, seq == 3);
  assert!(r.end_ms.unwrap() >= r.queued_ms);
 }
 // Executor can return cached Confirmed without entering insertion boundary.
 let index = observation::queued(7, 3, false).unwrap();
 observation::finish(Some(index), GuardedPasteOutcome::Confirmed { revision: 1 });
 let r = observation::snapshot().records[index].clone();
 assert!(r.insertion_start_ms.is_none()); assert!(!r.insertion_confirmed);
}
'''
# Exercise the exact legacy native entry point with native effects substituted.
saved = s
s = (root / 'src-tauri/src/infrastructure/auto_paste.rs').read_text()
legacy_boundary = block('pub fn paste_text_for_target(text: &str, target: &AutoPasteTarget)')
s = saved
code += legacy_boundary.replace('super::continuation_context::observation', 'observation')
code += r'''
#[derive(Clone, Copy)] enum AutoPasteMethod { Accessibility, Typed, Clipboard }
struct AutoPasteTarget;
type Result<T> = std::result::Result<T, ()>;
static LEGACY_FAIL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn paste_text_for_target_with<A,F>(_: &AutoPasteTarget, a:A, _:F) -> Result<AutoPasteMethod>
where A:FnOnce()->Result<bool>, F:FnOnce()->Result<AutoPasteMethod> {
 a()?; Ok(AutoPasteMethod::Accessibility)
}
fn paste_text_via_focused_accessibility_element(_: &str, _: &AutoPasteTarget)->Result<bool> {
 let trace = observation::snapshot();
 let r = trace.legacy_attempts.last().unwrap();
 assert!(r.end_ms.is_none()); assert!(r.result.is_none());
 if LEGACY_FAIL.load(std::sync::atomic::Ordering::Relaxed) { Err(()) } else { Ok(true) }
}
fn paste_text_hybrid_for_target(_: &str, _: &AutoPasteTarget)->Result<AutoPasteMethod> { Ok(AutoPasteMethod::Clipboard) }
#[test] fn legacy_real_boundary_retains_optional_session_and_uncertainty() {
 observation::with_legacy_session(Some(41), || {
  paste_text_for_target("never retained", &AutoPasteTarget).unwrap();
  observation::with_legacy_session(Some(42), || {
   LEGACY_FAIL.store(true, std::sync::atomic::Ordering::Relaxed);
   assert!(paste_text_for_target("not retained", &AutoPasteTarget).is_err());
  });
  LEGACY_FAIL.store(false, std::sync::atomic::Ordering::Relaxed);
  paste_text_for_target("", &AutoPasteTarget).unwrap();
 });
 paste_text_for_target("", &AutoPasteTarget).unwrap();
 let trace = observation::snapshot();
 let rows = &trace.legacy_attempts;
 assert_eq!(rows.iter().map(|r| r.session_id).collect::<Vec<_>>(), vec![Some(41),Some(42),Some(41),None]);
 assert_eq!(rows[0].result.as_deref(), Some("accessibility-api-ok"));
 assert_eq!(rows[1].result.as_deref(), Some("error-insertion-unknown"));
 assert!(rows.iter().all(|r| !r.insertion_confirmed && r.end_ms.unwrap() >= r.start_ms));
 // Panic/unwind restores scope and retains a pending effect as unknown.
 let _ = std::panic::catch_unwind(|| observation::with_legacy_session(Some(99), || {
  observation::legacy_start(); panic!("test unwind");
 }));
 let pending = observation::legacy_start().unwrap();
 assert_eq!(observation::snapshot().legacy_attempts[pending].session_id, None);
 for _ in 0..observation::LIMIT { observation::legacy_start(); }
 assert!(observation::snapshot().overflow);
 assert_eq!(observation::snapshot().legacy_attempts.len(), observation::LIMIT);
}
'''
with tempfile.TemporaryDirectory(prefix='p4-native-trace-') as directory:
    source = Path(directory) / 'trace.rs'
    source.write_text(code)
    binary = Path(directory) / 'trace-test'
    subprocess.run(['/root/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc', '--edition=2021', '--test', '--cfg', 'feature="native-window-e2e"', str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary), '--test-threads=1'], check=True)

# Compile the actual fixture implementation/regressions independently of Tauri.
# Keep Tokio real (already installed artifacts); only domain containers and JSON
# evidence serialization are stubbed. No Cargo or dependency resolution occurs.
native = (root / 'src-tauri/src/presentation/native_e2e.rs').read_text()
def extract(source, start):
    global s
    saved, s = s, source
    result = block(start)
    s = saved
    return result
fixture_parts = []
for name in ['struct AudioMarker', 'struct MarkerRange', 'struct ProviderMarkerRange',
             'struct CaptureRunAssociation']:
    fixture_parts.append('#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n' + extract(native, name))
for name in ['struct Counters', 'struct Timing', 'struct FakeContinuation']:
    fixture_parts.append('#[derive(Default)]\n' + extract(native, name))
for name in ['fn record_marker_violation', 'fn encode_audio_marker', 'fn decode_audio_marker',
             'struct FixtureProvider', 'impl Drop for FixtureProvider', 'impl FixtureProvider',
             'impl SttProvider for FixtureProvider', 'mod drain_tests']:
    fixture_parts.append(extract(native, name).replace('impl SttProvider for FixtureProvider', 'impl FixtureProvider'))
models = (root / 'src-tauri/src/domain/models/continuation.rs').read_text().split('// The negotiated wire')[0]
models += (root / 'src-tauri/src/domain/models/stt_completion.rs').read_text()
models = re.sub(r'^//!.*\n', '', models, flags=re.M)
models = models.replace('use serde::{Deserialize, Serialize};', '')
models = re.sub(r'#\[serde\([^\n]*\)\]\n', '', models).replace(', Serialize, Deserialize', '')
fixture_code = '''#![allow(dead_code, unused_variables)]
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[derive(Default, Clone)] struct Value;
impl std::ops::Index<&str> for Value { type Output=Value; fn index(&self, _: &str)->&Value { self } }
impl std::ops::IndexMut<&str> for Value { fn index_mut(&mut self, _: &str)->&mut Value { self } }
macro_rules! json { ($($t:tt)*) => { Value }; }
#[derive(Default)] struct FirstPcmLatency;
#[derive(Default)] struct Fixture { counters: Mutex<Counters>, timing: Mutex<Timing> }
struct SttConfig;
#[derive(Debug)] enum SttError { Processing(String), Connection(SttConnectionError) }
#[derive(Debug)] struct SttConnectionError;
impl SttConnectionError { fn simple(_: &str)->Self { Self } }
type SttResult<T> = Result<T, SttError>;
struct Transcription { text: String, delivery_seq: Option<u64> }
impl Transcription {
 fn final_result(text: String)->Self { Self { text, delivery_seq: None } }
 fn partial(text: String)->Self { Self::final_result(text) }
}
type TranscriptionCallback = Arc<dyn Fn(Transcription) + Send + Sync>;
type ErrorCallback = Arc<dyn Fn(String) + Send + Sync>;
type ConnectionQualityCallback = Arc<dyn Fn(u64,u64) + Send + Sync>;
struct AudioChunk { data: Vec<i16> }
impl AudioChunk { fn new(data: Vec<i16>, _: u32, _: u16)->Self { Self { data } } }
const AUDIO_MARKER_MAGIC: u32 = 0x56A1_7E2E;
const AUDIO_MARKER_BITS: usize = 96;
const MAX_RECORDED_GENERATIONS: usize = 256;
const MAX_MARKER_VIOLATIONS: usize = 64;
''' + models + '\n'.join(fixture_parts)
with tempfile.TemporaryDirectory(prefix='p4-native-fixture-') as directory:
    source = Path(directory) / 'fixture.rs'
    source.write_text(fixture_code)
    binary = Path(directory) / 'fixture-test'
    deps = root.parent / 'check-target/debug/deps'
    tokios = sorted(deps.glob('libtokio-*.rlib'))
    if not tokios:
        raise RuntimeError('Existing Tokio artifacts required; no dependency actions permitted')
    failures = []
    for tokio in tokios:
        built = subprocess.run(['/root/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc', '--edition=2021', '--test', str(source), '-L', f'dependency={deps}', '--extern', f'tokio={tokio}', '-o', str(binary)], capture_output=True, text=True)
        if built.returncode == 0:
            break
        failures.append(built.stderr)
    else:
        raise RuntimeError('\n'.join(failures))
    subprocess.run([str(binary), '--test-threads=1'], check=True)
