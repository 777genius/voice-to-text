//! Lifetime fencing for delayed recording-window operations.
//! Visibility closures must execute on the native UI thread, where Tauri's
//! show/hide dispatch is synchronous. Never hold this lock while scheduling it.
use std::sync::Mutex;

#[derive(Default)]
pub struct RecordingWindowLifecycle {
    epoch: Mutex<u64>,
}

impl RecordingWindowLifecycle {
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

    pub fn show_if_current<E>(
        &self,
        expected: u64,
        show: impl FnOnce() -> Result<(), E>,
    ) -> Result<Option<u64>, E> {
        let mut epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        if *epoch != expected {
            return Ok(None);
        }
        *epoch = epoch
            .checked_add(1)
            .expect("recording window epoch exhausted");
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
            return Ok(false);
        }
        hide()?;
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
            lifecycle.show_if_current(suppressed, || -> Result<(), ()> {
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
            lifecycle.show_if_current(epoch, || -> Result<(), ()> {
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
            .show_if_current(epoch, || Ok::<_, ()>(()))
            .unwrap()
            .is_some());
    }

    #[test]
    fn current_restore_advances_epoch_and_invalidates_old_hide() {
        let lifecycle = RecordingWindowLifecycle::default();
        let old = lifecycle.start_intent();
        let restored = lifecycle
            .show_if_current(old, || Ok::<_, ()>(()))
            .unwrap()
            .unwrap();
        assert!(restored > old);
        assert!(!lifecycle.hide_if_current(old, || Ok::<_, ()>(())).unwrap());
    }
}
