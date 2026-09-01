//! Pure normalization of recording hotkey input into semantic gestures.
//!
//! The caller owns OS integration and timestamps. This module never reads a clock,
//! waits, spawns work, or infers a press after a delay.

pub const DOUBLE_SPACE_WINDOW_MS: u64 = 350;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GestureSource {
    GlobalShortcut,
    DoubleSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GestureId {
    source: GestureSource,
    sequence: u64,
}

impl GestureId {
    pub const fn source(self) -> GestureSource {
        self.source
    }

    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatcherGeneration(u64);

impl WatcherGeneration {
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Identity that must accompany a physical release or watcher completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PressHandle {
    gesture_id: GestureId,
    watcher_generation: WatcherGeneration,
}

impl PressHandle {
    pub const fn gesture_id(self) -> GestureId {
        self.gesture_id
    }

    pub const fn watcher_generation(self) -> WatcherGeneration {
        self.watcher_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HoldToken(PressHandle);

impl HoldToken {
    pub const fn press_handle(self) -> PressHandle {
        self.0
    }

    pub const fn gesture_id(self) -> GestureId {
        self.0.gesture_id
    }

    pub const fn watcher_generation(self) -> WatcherGeneration {
        self.0.watcher_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalHotkeyMode {
    Toggle,
    Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceOffReason {
    Shutdown,
    Sleep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureIntent {
    Toggle {
        gesture_id: GestureId,
    },
    HoldBegan {
        token: HoldToken,
    },
    HoldEnded {
        token: HoldToken,
    },
    ForceOff {
        reason: ForceOffReason,
        interrupted_hold: Option<HoldToken>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptedPress {
    pub handle: PressHandle,
    pub intent: GestureIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressResult {
    Accepted(AcceptedPress),
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseResult {
    /// Toggle releases only rearm the physical latch.
    Rearmed,
    HoldEnded(GestureIntent),
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherResult {
    Rearmed,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActivePress {
    handle: PressHandle,
    mode: PhysicalHotkeyMode,
}

#[derive(Debug, Default)]
pub struct RecordingHotkeyGestureNormalizer {
    next_global_sequence: u64,
    next_double_space_sequence: u64,
    next_watcher_generation: u64,
    active_press: Option<ActivePress>,
    double_space: DoubleSpaceState,
}

impl RecordingHotkeyGestureNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accepts at most one press until a matching release or watcher rearm.
    pub fn press(&mut self, mode: PhysicalHotkeyMode) -> PressResult {
        if self.active_press.is_some() {
            return PressResult::Duplicate;
        }

        let gesture_id = GestureId {
            source: GestureSource::GlobalShortcut,
            sequence: increment_nonzero(&mut self.next_global_sequence),
        };
        let handle = PressHandle {
            gesture_id,
            watcher_generation: WatcherGeneration(increment_nonzero(
                &mut self.next_watcher_generation,
            )),
        };
        self.active_press = Some(ActivePress { handle, mode });

        let intent = match mode {
            PhysicalHotkeyMode::Toggle => GestureIntent::Toggle { gesture_id },
            PhysicalHotkeyMode::Hold => GestureIntent::HoldBegan {
                token: HoldToken(handle),
            },
        };
        PressResult::Accepted(AcceptedPress { handle, intent })
    }

    /// Accepts an already-normalized discrete source such as the second Space
    /// keydown. It does not share the physical global-shortcut latch.
    pub fn discrete_toggle(&mut self, source: GestureSource) -> GestureIntent {
        let sequence = match source {
            GestureSource::GlobalShortcut => increment_nonzero(&mut self.next_global_sequence),
            GestureSource::DoubleSpace => increment_nonzero(&mut self.next_double_space_sequence),
        };
        GestureIntent::Toggle {
            gesture_id: GestureId { source, sequence },
        }
    }

    /// A release is semantic only for the exact active hold token.
    pub fn release(&mut self, handle: PressHandle) -> ReleaseResult {
        let Some(active) = self.active_press else {
            return ReleaseResult::Stale;
        };
        if active.handle != handle {
            return ReleaseResult::Stale;
        }

        self.active_press = None;
        match active.mode {
            PhysicalHotkeyMode::Toggle => ReleaseResult::Rearmed,
            PhysicalHotkeyMode::Hold => {
                let token = HoldToken(handle);
                ReleaseResult::HoldEnded(GestureIntent::HoldEnded { token })
            }
        }
    }

    /// Recovery may only clear the exact press it observed and never emits intent.
    pub fn physical_watcher_released(&mut self, handle: PressHandle) -> WatcherResult {
        if self
            .active_press
            .is_some_and(|active| active.handle == handle)
        {
            self.active_press = None;
            WatcherResult::Rearmed
        } else {
            WatcherResult::Stale
        }
    }

    /// Clears all gesture state and emits an explicit system force-off intent.
    pub fn force_off(&mut self, reason: ForceOffReason) -> GestureIntent {
        let interrupted_hold = self.active_press.and_then(|active| {
            (active.mode == PhysicalHotkeyMode::Hold).then_some(HoldToken(active.handle))
        });
        self.active_press = None;
        self.double_space.reset();
        GestureIntent::ForceOff {
            reason,
            interrupted_hold,
        }
    }

    pub fn double_space_key_down(
        &mut self,
        key: DoubleSpaceKey,
        timestamp_ms: u64,
        is_repeat: bool,
    ) -> Option<GestureIntent> {
        if !self.double_space.key_down(key, timestamp_ms, is_repeat) {
            return None;
        }

        Some(GestureIntent::Toggle {
            gesture_id: GestureId {
                source: GestureSource::DoubleSpace,
                sequence: increment_nonzero(&mut self.next_double_space_sequence),
            },
        })
    }

    pub fn double_space_key_up(&mut self, key: DoubleSpaceKey) {
        self.double_space.key_up(key);
    }

    pub fn reset_double_space(&mut self) {
        self.double_space.reset();
    }

    pub fn active_press(&self) -> Option<PressHandle> {
        self.active_press.map(|active| active.handle)
    }
}

fn increment_nonzero(value: &mut u64) -> u64 {
    *value = value.wrapping_add(1);
    if *value == 0 {
        *value = 1;
    }
    *value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModifierKey {
    Alt,
    AltGr,
    ControlLeft,
    ControlRight,
    MetaLeft,
    MetaRight,
    ShiftLeft,
    ShiftRight,
}

impl ModifierKey {
    const fn mask(self) -> u16 {
        1 << self as u16
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoubleSpaceKey {
    Space,
    Modifier(ModifierKey),
    Other,
}

#[derive(Debug, Default)]
struct DoubleSpaceState {
    last_space_press_ms: Option<u64>,
    space_is_down: bool,
    modifiers_down: u16,
}

impl DoubleSpaceState {
    fn key_down(&mut self, key: DoubleSpaceKey, timestamp_ms: u64, is_repeat: bool) -> bool {
        match key {
            DoubleSpaceKey::Modifier(modifier) => {
                self.modifiers_down |= modifier.mask();
                self.last_space_press_ms = None;
                false
            }
            DoubleSpaceKey::Other => {
                self.last_space_press_ms = None;
                false
            }
            DoubleSpaceKey::Space if is_repeat || self.space_is_down => false,
            DoubleSpaceKey::Space => {
                self.space_is_down = true;
                if self.modifiers_down != 0 {
                    self.last_space_press_ms = None;
                    return false;
                }

                let triggered = self.last_space_press_ms.is_some_and(|previous_ms| {
                    timestamp_ms >= previous_ms
                        && timestamp_ms - previous_ms <= DOUBLE_SPACE_WINDOW_MS
                });
                self.last_space_press_ms = if triggered { None } else { Some(timestamp_ms) };
                triggered
            }
        }
    }

    fn key_up(&mut self, key: DoubleSpaceKey) {
        match key {
            DoubleSpaceKey::Space => self.space_is_down = false,
            DoubleSpaceKey::Modifier(modifier) => self.modifiers_down &= !modifier.mask(),
            DoubleSpaceKey::Other => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(result: PressResult) -> AcceptedPress {
        match result {
            PressResult::Accepted(accepted) => accepted,
            PressResult::Duplicate => panic!("expected accepted press"),
        }
    }

    fn tap_space(
        normalizer: &mut RecordingHotkeyGestureNormalizer,
        at_ms: u64,
    ) -> Option<GestureIntent> {
        let intent = normalizer.double_space_key_down(DoubleSpaceKey::Space, at_ms, false);
        normalizer.double_space_key_up(DoubleSpaceKey::Space);
        intent
    }

    #[test]
    fn toggle_emits_once_per_physical_press_and_release_only_rearms() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let first = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));
        assert_eq!(
            first.intent,
            GestureIntent::Toggle {
                gesture_id: first.handle.gesture_id()
            }
        );
        assert_eq!(
            normalizer.press(PhysicalHotkeyMode::Toggle),
            PressResult::Duplicate
        );
        assert_eq!(normalizer.release(first.handle), ReleaseResult::Rearmed);

        let second = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));
        assert_ne!(first.handle.gesture_id(), second.handle.gesture_id());
    }

    #[test]
    fn two_cycles_twenty_milliseconds_apart_are_both_accepted() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let first = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));
        assert_eq!(normalizer.release(first.handle), ReleaseResult::Rearmed);
        // No timestamp or debounce exists in the physical gesture API.
        let second = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));
        assert_ne!(first.handle, second.handle);
    }

    #[test]
    fn duplicate_press_cannot_change_mode() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let first = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));
        assert_eq!(
            normalizer.press(PhysicalHotkeyMode::Hold),
            PressResult::Duplicate
        );
        assert_eq!(normalizer.release(first.handle), ReleaseResult::Rearmed);
    }

    #[test]
    fn matching_hold_release_emits_the_same_token() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let press = accepted(normalizer.press(PhysicalHotkeyMode::Hold));
        let token = match press.intent {
            GestureIntent::HoldBegan { token } => token,
            intent => panic!("unexpected intent: {intent:?}"),
        };
        assert_eq!(
            normalizer.release(token.press_handle()),
            ReleaseResult::HoldEnded(GestureIntent::HoldEnded { token })
        );
    }

    #[test]
    fn stale_release_does_not_end_a_new_hold() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let old = accepted(normalizer.press(PhysicalHotkeyMode::Hold));
        assert!(matches!(
            normalizer.release(old.handle),
            ReleaseResult::HoldEnded(_)
        ));
        let current = accepted(normalizer.press(PhysicalHotkeyMode::Hold));

        assert_eq!(normalizer.release(old.handle), ReleaseResult::Stale);
        assert_eq!(normalizer.active_press(), Some(current.handle));
        assert!(matches!(
            normalizer.release(current.handle),
            ReleaseResult::HoldEnded(_)
        ));
    }

    #[test]
    fn stale_watcher_cannot_rearm_a_new_gesture() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let old = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));
        assert_eq!(normalizer.release(old.handle), ReleaseResult::Rearmed);
        let current = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));

        assert_eq!(
            normalizer.physical_watcher_released(old.handle),
            WatcherResult::Stale
        );
        assert_eq!(normalizer.active_press(), Some(current.handle));
        assert_eq!(
            normalizer.physical_watcher_released(current.handle),
            WatcherResult::Rearmed
        );
        assert_eq!(normalizer.active_press(), None);
    }

    #[test]
    fn watcher_requires_both_same_gesture_and_same_generation() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let current = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));
        let wrong_generation = PressHandle {
            gesture_id: current.handle.gesture_id,
            watcher_generation: WatcherGeneration(
                current.handle.watcher_generation.value().wrapping_add(1),
            ),
        };
        let wrong_gesture = PressHandle {
            gesture_id: GestureId {
                source: GestureSource::GlobalShortcut,
                sequence: current.handle.gesture_id.sequence().wrapping_add(1),
            },
            watcher_generation: current.handle.watcher_generation,
        };

        assert_eq!(
            normalizer.physical_watcher_released(wrong_generation),
            WatcherResult::Stale
        );
        assert_eq!(
            normalizer.physical_watcher_released(wrong_gesture),
            WatcherResult::Stale
        );
        assert_eq!(normalizer.active_press(), Some(current.handle));
        assert_eq!(
            normalizer.physical_watcher_released(current.handle),
            WatcherResult::Rearmed
        );
    }

    #[test]
    fn watcher_rearms_interrupted_hold_without_emitting_hold_end() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let hold = accepted(normalizer.press(PhysicalHotkeyMode::Hold));
        assert_eq!(
            normalizer.physical_watcher_released(hold.handle),
            WatcherResult::Rearmed
        );
        assert_eq!(normalizer.release(hold.handle), ReleaseResult::Stale);
    }

    #[test]
    fn force_off_clears_latches_and_reports_interrupted_hold() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let hold = accepted(normalizer.press(PhysicalHotkeyMode::Hold));
        let token = HoldToken(hold.handle);
        assert_eq!(
            normalizer.force_off(ForceOffReason::Shutdown),
            GestureIntent::ForceOff {
                reason: ForceOffReason::Shutdown,
                interrupted_hold: Some(token),
            }
        );
        assert_eq!(normalizer.active_press(), None);
        assert!(matches!(
            normalizer.press(PhysicalHotkeyMode::Toggle),
            PressResult::Accepted(_)
        ));
    }

    #[test]
    fn sleep_force_off_clears_pending_double_space() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        assert_eq!(tap_space(&mut normalizer, 100), None);
        assert_eq!(
            normalizer.force_off(ForceOffReason::Sleep),
            GestureIntent::ForceOff {
                reason: ForceOffReason::Sleep,
                interrupted_hold: None,
            }
        );
        assert_eq!(tap_space(&mut normalizer, 120), None);
    }

    #[test]
    fn double_space_accepts_second_non_repeat_keydown_once() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        assert_eq!(tap_space(&mut normalizer, 1_000), None);
        let toggle = tap_space(&mut normalizer, 1_200).expect("second space should toggle");
        let gesture_id = match toggle {
            GestureIntent::Toggle { gesture_id } => gesture_id,
            intent => panic!("unexpected intent: {intent:?}"),
        };
        assert_eq!(gesture_id.source(), GestureSource::DoubleSpace);
        assert_eq!(tap_space(&mut normalizer, 1_250), None);
    }

    #[test]
    fn double_space_threshold_is_inclusive() {
        let mut at_boundary = RecordingHotkeyGestureNormalizer::new();
        assert_eq!(tap_space(&mut at_boundary, 10), None);
        assert!(tap_space(&mut at_boundary, 10 + DOUBLE_SPACE_WINDOW_MS).is_some());

        let mut over_boundary = RecordingHotkeyGestureNormalizer::new();
        assert_eq!(tap_space(&mut over_boundary, 10), None);
        assert_eq!(
            tap_space(&mut over_boundary, 11 + DOUBLE_SPACE_WINDOW_MS),
            None
        );
    }

    #[test]
    fn autorepeat_never_completes_double_space() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        assert_eq!(
            normalizer.double_space_key_down(DoubleSpaceKey::Space, 100, false),
            None
        );
        assert_eq!(
            normalizer.double_space_key_down(DoubleSpaceKey::Space, 110, true),
            None
        );
        assert_eq!(
            normalizer.double_space_key_down(DoubleSpaceKey::Space, 120, false),
            None
        );
        normalizer.double_space_key_up(DoubleSpaceKey::Space);
        assert!(tap_space(&mut normalizer, 130).is_some());
    }

    #[test]
    fn modifier_interrupts_double_space_and_is_tracked_until_release() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        assert_eq!(tap_space(&mut normalizer, 100), None);
        assert_eq!(
            normalizer.double_space_key_down(
                DoubleSpaceKey::Modifier(ModifierKey::ShiftLeft),
                110,
                false,
            ),
            None
        );
        assert_eq!(tap_space(&mut normalizer, 120), None);
        normalizer.double_space_key_up(DoubleSpaceKey::Modifier(ModifierKey::ShiftLeft));
        assert_eq!(tap_space(&mut normalizer, 130), None);
        assert!(tap_space(&mut normalizer, 140).is_some());
    }

    #[test]
    fn multiple_modifiers_are_tracked_independently() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        normalizer.double_space_key_down(
            DoubleSpaceKey::Modifier(ModifierKey::ShiftLeft),
            1,
            false,
        );
        normalizer.double_space_key_down(
            DoubleSpaceKey::Modifier(ModifierKey::ControlLeft),
            2,
            false,
        );
        normalizer.double_space_key_up(DoubleSpaceKey::Modifier(ModifierKey::ShiftLeft));
        assert_eq!(tap_space(&mut normalizer, 10), None);
        normalizer.double_space_key_up(DoubleSpaceKey::Modifier(ModifierKey::ControlLeft));
        assert_eq!(tap_space(&mut normalizer, 20), None);
        assert!(tap_space(&mut normalizer, 30).is_some());
    }

    #[test]
    fn non_space_key_interrupts_double_space() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        assert_eq!(tap_space(&mut normalizer, 100), None);
        assert_eq!(
            normalizer.double_space_key_down(DoubleSpaceKey::Other, 110, false),
            None
        );
        assert_eq!(tap_space(&mut normalizer, 120), None);
        assert!(tap_space(&mut normalizer, 130).is_some());
    }

    #[test]
    fn explicit_double_space_reset_interrupts_sequence_and_clears_keys() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        normalizer.double_space_key_down(DoubleSpaceKey::Modifier(ModifierKey::MetaLeft), 1, false);
        normalizer.reset_double_space();
        assert_eq!(tap_space(&mut normalizer, 10), None);
        assert!(tap_space(&mut normalizer, 20).is_some());
    }

    #[test]
    fn decreasing_timestamp_does_not_complete_double_space() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        assert_eq!(tap_space(&mut normalizer, 200), None);
        assert_eq!(tap_space(&mut normalizer, 100), None);
        assert!(tap_space(&mut normalizer, 150).is_some());
    }

    #[test]
    fn global_and_double_space_ids_have_typed_sources() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let global = accepted(normalizer.press(PhysicalHotkeyMode::Toggle));
        assert_eq!(
            global.handle.gesture_id().source(),
            GestureSource::GlobalShortcut
        );
        assert_eq!(normalizer.release(global.handle), ReleaseResult::Rearmed);

        assert_eq!(tap_space(&mut normalizer, 1), None);
        let double = tap_space(&mut normalizer, 2).expect("double space should toggle");
        let GestureIntent::Toggle { gesture_id } = double else {
            panic!("expected toggle")
        };
        assert_eq!(gesture_id.source(), GestureSource::DoubleSpace);
    }

    #[test]
    fn discrete_double_space_uses_its_own_monotonic_identity() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let first = normalizer.discrete_toggle(GestureSource::DoubleSpace);
        let second = normalizer.discrete_toggle(GestureSource::DoubleSpace);
        let (
            GestureIntent::Toggle { gesture_id: first },
            GestureIntent::Toggle { gesture_id: second },
        ) = (first, second)
        else {
            panic!("expected toggles")
        };
        assert_eq!(first.source(), GestureSource::DoubleSpace);
        assert!(second.sequence() > first.sequence());
    }
}
