"""Linux-only contracts over unchanged production Rust bodies, not native E2E.

No dependency resolution. Generated files and binary live in a temporary directory.
The small wrapper supplies only derives, constants and the target data container.
"""
from pathlib import Path
import os
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
CONTEXT = (ROOT / 'src-tauri/src/infrastructure/continuation_context.rs').read_text()
PASTE = (ROOT / 'src-tauri/src/infrastructure/auto_paste.rs').read_text()


def block(source, signature):
    start = source.index(signature)
    brace = source.index('{', start)
    depth, end = 1, brace + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


parts = ['#![allow(dead_code)]',
         '#[derive(Debug, PartialEq, Eq)] pub struct AutoPasteTarget { bundle_id: String, pid: i32 }']
for name in ['VOICETEXT_PROD_BUNDLE_ID', 'VOICETEXT_DEV_BUNDLE_ID', 'VOICETEXT_BUNDLE_IDS']:
    start = PASTE.index(f'const {name}:')
    parts.append(PASTE[start:PASTE.index(';', start) + 1])
start = CONTEXT.index('const MAX_TEXT_BYTES:')
parts.append(CONTEXT[start:CONTEXT.index(';', start) + 1])
for name in ['TextRange', 'LocalSnapshot']:
    parts.append('#[derive(Clone, Debug, PartialEq, Eq)]\n' + block(CONTEXT, f'pub(crate) struct {name}'))
parts += [block(CONTEXT, 'impl LocalSnapshot'),
          block(CONTEXT, 'pub(crate) fn permits_frontmost_validation'),
          block(PASTE, 'pub fn normalize_auto_paste_target'),
          block(PASTE, 'pub(crate) fn continuation_app_qualified')]
backend = (ROOT / 'src-tauri/src/infrastructure/stt/backend.rs').read_text()
start = backend.index('let offered_continuation = provider_name ==')
offer = backend[start:backend.index(';', start) + 1]
parts.append('struct Config { continuation_target_eligible: bool } struct Offer { continuation_opted_in: bool }')
parts.append('impl Offer { fn offered(&self, provider_name: &str, config: &Config) -> bool { '
             + offer + ' offered_continuation } }')
ready = block(backend, 'if offered_continuation\n')
parts.append('const CAPABILITY_PAUSE_CONTINUE: &str = "pause"; const CAPABILITY_FINALIZE_OUTCOME: &str = "outcome"; '
             'struct ContinuationSession { connection_generation: u64, provider_session_id: String } '
             'struct Negotiation { session: Option<ContinuationSession> } '
             'fn ready_mode(offered_continuation: bool) -> bool { '
             'let accepted_capabilities = [CAPABILITY_PAUSE_CONTINUE.to_string(), CAPABILITY_FINALIZE_OUTCOME.to_string()]; '
             'let mut continuation_delivery = false; let mut negotiation = Negotiation { session: None }; '
             'let connection_generation = 1; let session_id = String::new(); '
             + ready + ' continuation_delivery }')
tests = (ROOT / 'src-tauri/tests/native_gap_support/contracts.rs').read_text()
with tempfile.TemporaryDirectory(prefix='voicetext-native-gaps-') as directory:
    source, binary = Path(directory) / 'contracts.rs', Path(directory) / 'contracts-test'
    parts.append('mod contracts { use super::*;\n' + tests.replace('//!', '//') + '\n}')
    source.write_text('\n'.join(parts))
    rustc = os.environ.get('RUSTC') or shutil.which('rustc')
    if not rustc:
        raise RuntimeError('rustc required; no toolchain installation attempted')
    subprocess.run([rustc, '--edition=2021', '--test', str(source), '-o', str(binary)], check=True)
    result = subprocess.run([str(binary), '--test-threads=1', '--nocapture'], check=True, capture_output=True, text=True)
    print(result.stdout)
    modes = re.findall(r'E50_ELIGIBLE=(true|false)', result.stdout)
    if len(modes) != 3:
        raise RuntimeError('Missing Rust qualification/offer evidence')
    env = dict(os.environ, VT_GAP_ELIGIBILITY='[' + ','.join(modes) + ']')
    subprocess.run(['node', str(ROOT / 'e2e-tests/helpers/nativeGapRouting.mjs')], env=env, check=True)

# Second executable compiles the actual Mac capture body and gated fault/trace
# modules with deterministic platform ports. It is not a Mac compilation check.
scaffold = (ROOT / 'src-tauri/tests/native_gap_support/capture_contracts.rs').read_text()
scaffold = scaffold.replace('//!', '//')
# Select explicitly instead of relying on the source-contract list layout.
base = [part for part in parts if not part.startswith('mod contracts')
        and not part.startswith('struct Config') and not part.startswith('impl Offer')]
base[1] = base[1].replace('derive(Debug', 'derive(Clone, Debug')
observation = block(CONTEXT, 'pub(crate) mod observation')
observation = re.sub(r'#\[serde\([^\n]*\)\]\n', '', observation).replace(', Serialize', '')
base += [scaffold, observation, block(CONTEXT, 'pub mod native_e2e'),
         block(CONTEXT, 'pub(crate) struct NativeEligibilityBudget'),
         block(CONTEXT, 'impl NativeEligibilityBudget'),
         block(CONTEXT, 'impl Drop for NativeEligibilityBudget'),
         block(CONTEXT, 'pub(crate) fn native_timeout_seconds')]
native = PASTE[PASTE.index('pub(crate) struct ContinuationNativeContext {'):]
base += [block(native, 'pub(crate) struct ContinuationNativeContext'),
         'impl ContinuationNativeContext { ' + '\n'.join(block(native, signature) for signature in [
             'pub(crate) fn capture(target: AutoPasteTarget)', 'fn identity(&self)',
             'pub(crate) fn validate(&mut self)', 'pub(crate) fn paste(&mut self', 'fn insert_once(']) + ' }']
with tempfile.TemporaryDirectory(prefix='voicetext-native-capture-') as directory:
    source, binary = Path(directory) / 'capture.rs', Path(directory) / 'capture-test'
    source.write_text('\n'.join(base))
    subprocess.run([rustc, '--edition=2021', '--test', '--cfg', 'feature="native-window-e2e"',
                    str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary), '--test-threads=1'], check=True)
