//! Platform ports for source-contract compilation only. No native APIs are called.
extern crate self as anyhow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
pub type Result<T> = std::result::Result<T, String>;
#[macro_export]
macro_rules! anyhow {
    ($message:expr) => {
        $message.to_string()
    };
}
#[macro_export]
macro_rules! bail {
    ($message:expr) => {
        return Err($message.to_string())
    };
}
#[macro_export]
macro_rules! ensure {
    ($ok:expr, $message:expr) => {
        if !$ok {
            bail!($message);
        }
    };
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GuardedPasteOutcome {
    Confirmed { revision: u64 },
    Unavailable,
    ContextMismatch,
    Uncertain,
}
struct Execution {
    trace: Option<usize>,
}
thread_local! {
    static EXECUTION: std::cell::RefCell<Option<Execution>> = const { std::cell::RefCell::new(None) };
    static NATIVE_DEADLINE: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}
const ELIGIBILITY_TIMEOUT: Duration = Duration::from_millis(100);
mod infrastructure {
    pub mod continuation_context {
        pub(crate) use crate::{
            begin_effect, clipboard_delivery, insert_with_current_clipboard,
            permits_frontmost_validation,
        };
        pub(crate) use crate::{native_e2e, native_timeout_seconds, NativeEligibilityBudget};
    }
}
type MacCFTypeRef = *const u8;
type MacAXUIElementRef = *const u8;
type DocumentIdentity = ();
type Snapshot = LocalSnapshot;
struct Owned(MacCFTypeRef);
impl Owned {
    fn bounded(&self) -> bool {
        native_timeout_seconds().is_some()
    }
    fn attribute(&self, _: &str) -> Option<Self> {
        Some(Self(self.0))
    }
}
#[allow(non_snake_case)]
unsafe fn AXUIElementCreateApplication(_: i32) -> MacCFTypeRef {
    std::ptr::NonNull::dangling().as_ptr()
}
fn check_accessibility_permission() -> bool {
    true
}
fn frontmost_app_matches_target(target: &AutoPasteTarget) -> bool {
    FRONT
        .lock()
        .unwrap()
        .as_ref()
        .is_none_or(|front| front == target)
}
fn focused_element_pid(_: MacAXUIElementRef) -> Option<i32> {
    Some(42)
}
fn bounded_string_attribute(_: &Owned, _: &str) -> Option<Option<Vec<u16>>> {
    Some(None)
}
fn ax_attribute_settable(_: MacAXUIElementRef, _: &str) -> Option<bool> {
    Some(true)
}
fn document_identity(_: &Owned) -> Option<()> {
    Some(())
}
static SNAPSHOTS: AtomicUsize = AtomicUsize::new(0);
fn snapshot(_: &Owned) -> Option<Snapshot> {
    SNAPSHOTS.fetch_add(1, Ordering::SeqCst);
    Some(LocalSnapshot {
        selection: TextRange {
            location: 0,
            length: 0,
        },
        total: 0,
        start: 0,
        anchor: vec![],
    })
}
fn target() -> AutoPasteTarget {
    AutoPasteTarget {
        bundle_id: "com.apple.TextEdit".into(),
        pid: 42,
    }
}

#[test]
fn e35_actual_capture_branches_refuse_before_text_snapshot_and_restore_budget() {
    use native_e2e::{arm_capture_fault, CaptureFault::*};
    assert!(ContinuationNativeContext::capture(target()).is_ok());
    assert_eq!(SNAPSHOTS.swap(0, Ordering::SeqCst), 1);
    for kind in [Unsupported, Revoked, Secure, Timeout] {
        let fault = arm_capture_fault(target(), kind).unwrap();
        assert!(ContinuationNativeContext::capture(target()).is_err());
        assert!(fault.was_consumed());
        assert_eq!(SNAPSHOTS.load(Ordering::SeqCst), 0);
        assert!(NATIVE_DEADLINE.with(|d| d.get()).is_none());
    }
    assert!(ContinuationNativeContext::capture(target()).is_ok());
    assert_eq!(SNAPSHOTS.swap(0, Ordering::SeqCst), 1);
}

#[test]
fn capture_fault_target_matching_and_drop_do_not_poison_next_capture() {
    use native_e2e::*;
    let armed = arm_capture_fault(target(), CaptureFault::Secure).unwrap();
    let mut other = target();
    other.pid += 1;
    assert!(take_capture_fault(&other).is_none());
    assert!(!armed.was_consumed());
    assert!(arm_capture_fault(target(), CaptureFault::Revoked).is_err());
    drop(armed);
    assert!(take_capture_fault(&target()).is_none());
    let first = arm_capture_fault(target(), CaptureFault::Revoked).unwrap();
    assert_eq!(take_capture_fault(&target()), Some(CaptureFault::Revoked));
    let next = arm_capture_fault(target(), CaptureFault::Secure).unwrap();
    drop(first);
    assert_eq!(take_capture_fault(&target()), Some(CaptureFault::Secure));
    assert!(next.was_consumed());
}

#[test]
fn readback_faults_match_run_sequence_kind_and_are_one_shot() {
    use native_e2e::*;
    let fault = arm_post_paste_readback_mismatch(31, 2).unwrap();
    for (run, seq) in [(30, 2), (31, 1)] {
        let trace = observation::queued(run, seq, false);
        EXECUTION.with(|e| *e.borrow_mut() = Some(Execution { trace }));
        assert!(!take_post_paste_readback_mismatch());
    }
    EXECUTION.with(|e| {
        *e.borrow_mut() = Some(Execution {
            trace: observation::queued(31, 2, false),
        })
    });
    assert!(!take_post_paste_readback_unavailable());
    assert!(take_post_paste_readback_mismatch());
    assert!(!take_post_paste_readback_mismatch());
    assert!(fault.was_consumed());
    let unavailable = arm_post_paste_readback_unavailable(31, 2).unwrap();
    drop(fault);
    assert!(!take_post_paste_readback_mismatch());
    assert!(take_post_paste_readback_unavailable());
    assert!(unavailable.was_consumed());
    drop(arm_post_paste_readback_mismatch(31, 2).unwrap());
    assert!(!take_post_paste_readback_mismatch());
    EXECUTION.with(|e| *e.borrow_mut() = None);
}

#[derive(Clone, Copy)]
enum ContextValidation {
    Valid { revision: u64 },
    Mismatch,
    Unavailable,
}
static FRONT: Mutex<Option<AutoPasteTarget>> = Mutex::new(None);
static PASTED: Mutex<Vec<AutoPasteTarget>> = Mutex::new(Vec::new());
fn frontmost_identity_for_validation() -> Option<AutoPasteTarget> {
    FRONT.lock().unwrap().clone()
}
#[allow(non_snake_case)]
unsafe fn CFEqual(a: MacCFTypeRef, b: MacCFTypeRef) -> bool {
    a == b
}
fn activate_continuation_target(target: &AutoPasteTarget) -> Result<()> {
    *FRONT.lock().unwrap() = Some(target.clone());
    Ok(())
}
fn begin_effect() -> bool {
    true
}
struct Clipboard;
impl Clipboard {
    fn new() -> Result<Self> {
        Ok(Self)
    }
}
fn target_can_safely_restore_clipboard(_: &AutoPasteTarget) -> bool {
    true
}
fn clipboard_delivery(
    c: &mut Clipboard,
    _: &str,
    _: bool,
    insert: impl FnOnce(&mut Clipboard, i64) -> GuardedPasteOutcome,
) -> GuardedPasteOutcome {
    insert(c, 1)
}
fn insert_with_current_clipboard(
    _: &mut Clipboard,
    _: i64,
    insert: impl FnOnce() -> GuardedPasteOutcome,
) -> GuardedPasteOutcome {
    insert()
}
impl ContinuationNativeContext {
    fn post_and_confirm(&mut self, _: &str, _: i64) -> GuardedPasteOutcome {
        assert_eq!(FRONT.lock().unwrap().as_ref(), Some(&self.target));
        PASTED.lock().unwrap().push(self.target.clone());
        GuardedPasteOutcome::Confirmed { revision: 0 }
    }
}
#[test]
fn e36_real_validation_and_paste_reactivate_retained_editor_from_mini_window() {
    let mut context = ContinuationNativeContext::capture(target()).unwrap();
    for bundle in VOICETEXT_BUNDLE_IDS {
        *FRONT.lock().unwrap() = Some(AutoPasteTarget {
            bundle_id: (*bundle).into(),
            pid: std::process::id() as i32,
        });
        assert_eq!(
            context.paste("owned"),
            GuardedPasteOutcome::Confirmed { revision: 0 }
        );
        assert_eq!(context.target, target());
    }
    assert_eq!(*PASTED.lock().unwrap(), vec![target(), target()]);
    *FRONT.lock().unwrap() = Some(AutoPasteTarget {
        bundle_id: VOICETEXT_BUNDLE_IDS[0].into(),
        pid: std::process::id() as i32 + 1,
    });
    assert_eq!(context.paste("owned"), GuardedPasteOutcome::ContextMismatch);
    assert_eq!(PASTED.lock().unwrap().len(), 2);
    *FRONT.lock().unwrap() = None;
}
