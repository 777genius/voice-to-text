//! Lifetime fencing for delayed recording-window operations.
//! Visibility closures must execute on the native UI thread, where Tauri's
//! show/hide dispatch is synchronous. Never hold this lock while scheduling it.
use std::{collections::BTreeMap, sync::Mutex};

#[derive(Default)]
pub struct RecordingWindowLifecycle {
    epoch: Mutex<u64>,
    session_epochs: Mutex<BTreeMap<u64, u64>>,
}

impl RecordingWindowLifecycle {
    /// Only capture startup may acquire a visibility lease, never terminal delivery.
    pub fn bind_session(&self, session_id: u64) {
        let epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        let mut leases = self
            .session_epochs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        leases.entry(session_id).or_insert(*epoch);
        while leases.len() > 128 {
            leases.pop_first();
        }
    }

    /// A native show may bind or transfer a lease only after its foreground-run
    /// owner has been verified outside this lifecycle primitive.
    pub fn bind_or_transfer_session_if_current(
        &self,
        session_id: u64,
        expected_epoch: u64,
    ) -> bool {
        let epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        if *epoch != expected_epoch {
            return false;
        }
        let mut leases = self
            .session_epochs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        leases.insert(session_id, expected_epoch);
        while leases.len() > 128 {
            leases.pop_first();
        }
        true
    }

    pub fn session_epoch(&self, session_id: u64) -> Option<u64> {
        self.session_epochs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&session_id)
            .copied()
    }

    pub fn current(&self) -> u64 {
        *self.epoch.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn start_intent(&self) -> u64 {
        let mut epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        *epoch = epoch
            .checked_add(1)
            .expect("recording window epoch exhausted");
        *epoch
    }

    pub fn show<E>(&self, show: impl FnOnce() -> Result<(), E>) -> Result<u64, E> {
        let mut epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        *epoch = epoch
            .checked_add(1)
            .expect("recording window epoch exhausted");
        show()?;
        Ok(*epoch)
    }

    pub fn restore_if_current<E>(
        &self,
        expected: u64,
        show: impl FnOnce() -> Result<(), E>,
    ) -> Result<Option<u64>, E> {
        let epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        if *epoch != expected {
            return Ok(None);
        }
        show()?;
        Ok(Some(*epoch))
    }

    pub fn restore_if_owned_by_session<E>(
        &self,
        session_id: u64,
        expected: u64,
        show: impl FnOnce() -> Result<(), E>,
    ) -> Result<Option<u64>, E> {
        let epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        if *epoch != expected {
            return Ok(None);
        }
        let leases = self
            .session_epochs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if leases.get(&session_id).copied() != Some(expected) {
            return Ok(None);
        }
        show()?;
        Ok(Some(*epoch))
    }

    pub fn hide_if_current<E>(
        &self,
        expected: u64,
        hide: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        if *epoch != expected {
            log::debug!(
                "recording window close rejected: expected_epoch={}, current_epoch={}",
                expected,
                *epoch
            );
            return Ok(false);
        }
        hide()?;
        log::debug!("recording window native hide committed: epoch={}", expected);
        // A final close revokes temporary suppression/restore and all old timers.
        *epoch = epoch
            .checked_add(1)
            .expect("recording window epoch exhausted");
        Ok(true)
    }

    pub fn while_current<E>(
        &self,
        expected: u64,
        hide: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        let epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        if *epoch != expected {
            return Ok(false);
        }
        hide()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn terminal_delivery_cannot_acquire_reopened_successor_window() {
        let lifecycle = RecordingWindowLifecycle::default();
        let old = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        lifecycle.bind_session(10);
        lifecycle.hide_if_current(old, || Ok::<_, ()>(())).unwrap();
        let reopened = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        lifecycle.bind_session(11);
        // Delayed A delivery must still hold A's epoch, even after B starts.
        lifecycle.bind_session(10);
        assert_eq!(lifecycle.session_epoch(10), Some(old));
        assert_eq!(lifecycle.session_epoch(11), Some(reopened));
        assert!(!lifecycle
            .while_current(
                lifecycle.session_epoch(10).unwrap(),
                || -> Result<(), ()> { panic!("old paste must not suppress successor") }
            )
            .unwrap());
    }

    #[test]
    fn session_paste_without_reopen_can_suppress_and_restore() {
        let lifecycle = RecordingWindowLifecycle::default();
        lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        lifecycle.bind_session(10);
        let lease = lifecycle.session_epoch(10).unwrap();
        assert!(lifecycle.while_current(lease, || Ok::<_, ()>(())).unwrap());
        let restored = lifecycle
            .restore_if_owned_by_session(10, lease, || Ok::<_, ()>(()))
            .unwrap()
            .unwrap();
        assert_eq!(restored, lease);
        assert_eq!(lifecycle.session_epoch(10), Some(restored));
        assert_eq!(lifecycle.session_epoch(99), None);
        assert!(lifecycle
            .hide_if_current(lease, || Ok::<_, ()>(()))
            .unwrap());
        assert_eq!(
            lifecycle.restore_if_owned_by_session(10, lease, || -> Result<(), ()> {
                panic!("final close must revoke session restoration")
            }),
            Ok(None)
        );
    }

    #[test]
    fn restore_cannot_transfer_an_epoch_not_owned_by_its_session() {
        let lifecycle = RecordingWindowLifecycle::default();
        let old = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        lifecycle.bind_session(10);
        lifecycle.hide_if_current(old, || Ok::<_, ()>(())).unwrap();
        let successor = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        lifecycle.bind_session(11);
        assert_eq!(
            lifecycle.restore_if_owned_by_session(10, successor, || -> Result<(), ()> {
                panic!("old session restored a successor window")
            }),
            Ok(None)
        );
        assert_eq!(lifecycle.session_epoch(10), Some(old));
        assert_eq!(lifecycle.session_epoch(11), Some(successor));
    }

    #[test]
    fn verified_active_session_can_take_its_reopened_epoch() {
        let lifecycle = RecordingWindowLifecycle::default();
        lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        lifecycle.bind_session(10);
        let reopened = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        assert!(lifecycle.bind_or_transfer_session_if_current(10, reopened));
        assert_eq!(lifecycle.session_epoch(10), Some(reopened));
        assert!(!lifecycle.bind_or_transfer_session_if_current(10, reopened - 1));
        assert!(lifecycle.bind_or_transfer_session_if_current(99, reopened));
    }

    #[test]
    fn hide_queued_before_new_show_cannot_hide_it_at_commit() {
        let lifecycle = RecordingWindowLifecycle::default();
        let visible = Cell::new(false);
        let old = lifecycle
            .show(|| {
                visible.set(true);
                Ok::<_, ()>(())
            })
            .unwrap();
        let deferred_hide = || {
            lifecycle.hide_if_current(old, || {
                visible.set(false);
                Ok::<_, ()>(())
            })
        };
        lifecycle
            .show(|| {
                visible.set(true);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(!deferred_hide().unwrap());
        assert!(visible.get());
    }

    #[test]
    fn delayed_hotkey_hide_is_invalidated_by_hidden_start() {
        let lifecycle = RecordingWindowLifecycle::default();
        let old = lifecycle.start_intent();
        lifecycle.start_intent();
        assert!(!lifecycle
            .hide_if_current(old, || -> Result<(), ()> { panic!("stale hide committed") })
            .unwrap());
    }

    #[test]
    fn repeated_stop_and_open_only_allows_current_hide() {
        let lifecycle = RecordingWindowLifecycle::default();
        for _ in 0..100 {
            let old = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
            let current = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
            assert!(!lifecycle.hide_if_current(old, || Ok::<_, ()>(())).unwrap());
            assert!(lifecycle
                .hide_if_current(current, || Ok::<_, ()>(()))
                .unwrap());
        }
    }
}

/// A queued hotkey keeps the action it originally requested. In particular a
/// delayed release can only stop its session; it can never become a start.
#[derive(Clone, Copy, Debug)]
pub enum RecordingHotkeyAction {
    Start { press_seq: u64 },
    Stop { session_id: u64, window_epoch: u64 },
}

impl RecordingHotkeyAction {
    pub fn eligible(
        self,
        latest_press: u64,
        current_session: u64,
        can_start: bool,
        can_stop: bool,
    ) -> bool {
        match self {
            Self::Start { press_seq } => press_seq == latest_press && can_start,
            Self::Stop { session_id, .. } => {
                session_id != 0 && session_id == current_session && can_stop
            }
        }
    }
}

#[cfg(test)]
mod hotkey_tests {
    use super::*;

    #[test]
    fn duplicate_or_delayed_release_never_reopens_idle_window() {
        let stop = RecordingHotkeyAction::Stop {
            session_id: 4,
            window_epoch: 9,
        };
        assert!(stop.eligible(1, 4, false, true));
        assert!(!stop.eligible(1, 0, true, false));
        assert!(!stop.eligible(1, 4, true, false));
        assert!(!stop.eligible(2, 5, false, true));
    }

    #[test]
    fn newest_start_survives_queue_but_never_stops_active_session() {
        let pending = (1..=3)
            .map(|press_seq| RecordingHotkeyAction::Start { press_seq })
            .collect::<Vec<_>>();
        assert_eq!(
            pending
                .iter()
                .filter(|action| action.eligible(3, 0, true, false))
                .count(),
            1
        );
        assert!(!pending[2].eligible(3, 6, false, true));
    }

    #[test]
    fn rapid_stop_then_start_preserves_old_stop_ownership() {
        let stop = RecordingHotkeyAction::Stop {
            session_id: 4,
            window_epoch: 9,
        };
        // New press does not revoke the stop already accepted for the old session.
        assert!(stop.eligible(2, 4, false, true));
        let start = RecordingHotkeyAction::Start { press_seq: 2 };
        assert!(start.eligible(2, 0, true, false));
    }
}

#[derive(Default)]
pub struct PendingRecordingStart(std::sync::atomic::AtomicU64);

impl PendingRecordingStart {
    pub fn replace(&self, owner: u64) {
        self.0.store(owner, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn clear(&self) {
        self.replace(0);
    }

    pub fn is_current(&self, owner: u64) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst) == owner
    }

    pub fn take_if_current(&self, owner: u64) -> bool {
        self.0
            .compare_exchange(
                owner,
                0,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }
}

#[cfg(test)]
mod pending_tests {
    use super::*;

    #[test]
    fn superseded_restart_cannot_clear_or_execute_new_owner() {
        let pending = PendingRecordingStart::default();
        pending.replace(1);
        pending.replace(2);
        assert!(!pending.take_if_current(1));
        assert!(pending.is_current(2));
        assert!(pending.take_if_current(2));
        assert!(!pending.take_if_current(2));
    }
}

/// Remembers a stop already requested while its native hide/finalize is pending.
/// A second toggle then requests restart even if the service still says Recording.
#[derive(Default)]
pub struct RecordingHotkeyIntents {
    state: Mutex<RecordingHotkeyIntentState>,
}

#[derive(Default)]
struct RecordingHotkeyIntentState {
    last_selected_press: u64,
    pending_stop: Option<(u64, u64)>,
}

impl RecordingHotkeyIntents {
    pub fn toggle(
        &self,
        press_seq: u64,
        session_id: u64,
        window_epoch: u64,
        can_stop: bool,
    ) -> Option<RecordingHotkeyAction> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if press_seq <= state.last_selected_press {
            return None;
        }
        state.last_selected_press = press_seq;
        Some(
            if can_stop
                && !state
                    .pending_stop
                    .is_some_and(|(owner, session)| owner < press_seq && session == session_id)
            {
                state.pending_stop = Some((press_seq, session_id));
                RecordingHotkeyAction::Stop {
                    session_id,
                    window_epoch,
                }
            } else {
                RecordingHotkeyAction::Start { press_seq }
            },
        )
    }

    pub fn finish_stop(&self, press_seq: u64, session_id: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.pending_stop == Some((press_seq, session_id)) {
            state.pending_stop = None;
        }
    }
}

#[cfg(test)]
mod intent_tests {
    use super::*;

    #[test]
    fn toggle_during_suspended_stop_requests_restart_not_another_stop() {
        let intents = RecordingHotkeyIntents::default();
        let stop = intents.toggle(1, 7, 4, true).unwrap();
        // First stop is still in its UI flush delay; backend status is Recording.
        let restart = intents.toggle(2, 7, 4, true).unwrap();
        assert!(matches!(
            stop,
            RecordingHotkeyAction::Stop { session_id: 7, .. }
        ));
        assert!(matches!(
            restart,
            RecordingHotkeyAction::Start { press_seq: 2 }
        ));
        assert!(stop.eligible(2, 7, false, true));
        intents.finish_stop(1, 7);
        assert!(restart.eligible(2, 0, true, false));
    }

    #[test]
    fn delayed_old_selector_cannot_overwrite_new_pending_stop() {
        let intents = RecordingHotkeyIntents::default();
        assert!(matches!(
            intents.toggle(2, 7, 4, true),
            Some(RecordingHotkeyAction::Stop { .. })
        ));
        assert!(intents.toggle(1, 7, 4, true).is_none());
        intents.finish_stop(1, 7);
        assert!(matches!(
            intents.toggle(3, 7, 4, true),
            Some(RecordingHotkeyAction::Start { press_seq: 3 })
        ));
    }

    #[test]
    fn queued_restarts_only_execute_latest_accepted_press() {
        let intents = RecordingHotkeyIntents::default();
        intents.toggle(1, 7, 4, true);
        let old = intents.toggle(2, 7, 4, true).unwrap();
        let latest = intents.toggle(3, 7, 4, true).unwrap();
        assert!(!old.eligible(3, 0, true, false));
        assert!(latest.eligible(3, 0, true, false));
    }
}

pub fn recording_stop_is_current(expected_session: Option<u64>, active_session: u64) -> bool {
    expected_session.map_or(true, |expected| expected != 0 && expected == active_session)
}

#[cfg(test)]
mod commit_tests {
    use super::*;

    #[test]
    fn delayed_ui_stop_cannot_stop_replacement_session() {
        assert!(recording_stop_is_current(Some(7), 7));
        assert!(!recording_stop_is_current(Some(7), 8));
        assert!(!recording_stop_is_current(Some(7), 0));
        assert!(!recording_stop_is_current(Some(0), 0));
    }

    #[test]
    fn automatic_restore_cannot_reopen_newer_window_lifetime() {
        let lifecycle = RecordingWindowLifecycle::default();
        let suppressed = lifecycle.start_intent();
        lifecycle.start_intent();
        assert_eq!(
            lifecycle.restore_if_current(suppressed, || -> Result<(), ()> {
                panic!("old auto-paste restore reopened replacement window")
            }),
            Ok(None)
        );
    }

    #[test]
    fn final_hide_revokes_pending_temporary_restore() {
        let lifecycle = RecordingWindowLifecycle::default();
        let epoch = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        assert!(lifecycle.while_current(epoch, || Ok::<_, ()>(())).unwrap());
        assert!(lifecycle
            .hide_if_current(epoch, || Ok::<_, ()>(()))
            .unwrap());
        assert_eq!(
            lifecycle.restore_if_current(epoch, || -> Result<(), ()> {
                panic!("restored after close")
            }),
            Ok(None)
        );
    }

    #[test]
    fn temporary_hide_alone_keeps_restore_authority() {
        let lifecycle = RecordingWindowLifecycle::default();
        let epoch = lifecycle.show(|| Ok::<_, ()>(())).unwrap();
        assert!(lifecycle.while_current(epoch, || Ok::<_, ()>(())).unwrap());
        assert!(lifecycle
            .restore_if_current(epoch, || Ok::<_, ()>(()))
            .unwrap()
            .is_some());
    }

    #[test]
    fn temporary_restore_preserves_pending_close() {
        let lifecycle = RecordingWindowLifecycle::default();
        let old = lifecycle.start_intent();
        let restored = lifecycle
            .restore_if_current(old, || Ok::<_, ()>(()))
            .unwrap()
            .unwrap();
        assert_eq!(restored, old);
        assert!(lifecycle.hide_if_current(old, || Ok::<_, ()>(())).unwrap());
    }
}
