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
pub enum PhysicalObservation {
    Down,
    Up,
    Unavailable,
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
struct ActivePress {
    handle: PressHandle,
    mode: PhysicalHotkeyMode,
    supported: bool,
    interrupted: bool,
    physical_binding: Option<(u16, u8)>,
}

#[derive(Debug, Default)]
pub struct RecordingHotkeyGestureNormalizer {
    next_global_sequence: u64,
    next_double_space_sequence: u64,
    next_watcher_generation: u64,
    active_press: Option<ActivePress>,
    /// A watcher can observe key-up before the shortcut plugin delivers the
    /// corresponding Released callback. Those callbacks must be consumed as
    /// releases of the retired press, never applied to a newer press.
    recovered_release_debt: u64,
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
        self.active_press = Some(ActivePress {
            handle,
            mode,
            supported: false,
            interrupted: false,
            physical_binding: None,
        });

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

    /// Sample under the caller's latch lock. Up on a Pressed callback is not
    /// evidence of a new down edge; it may only clear an interrupted latch.
    pub fn press_observed(
        &mut self,
        mode: PhysicalHotkeyMode,
        observation: PhysicalObservation,
    ) -> PressResult {
        if observation == PhysicalObservation::Up {
            if self.active_press.is_some_and(|p| p.interrupted) {
                self.active_press = None;
            }
            return PressResult::Duplicate;
        }
        let result = self.press(mode);
        if let PressResult::Accepted(_) = result {
            self.active_press.as_mut().unwrap().supported =
                observation == PhysicalObservation::Down;
        }
        result
    }

    /// Adapter-owned chord descriptor, frozen with the exact accepted handle.
    /// Re-registration must not reinterpret an older latch using a new chord.
    pub fn bind_physical_chord(&mut self, handle: PressHandle, binding: (u16, u8)) {
        if let Some(active) = self.active_press.as_mut() {
            if active.handle == handle && active.physical_binding.is_none() {
                active.physical_binding = Some(binding);
            }
        }
    }

    pub fn physical_binding(&self) -> Option<(u16, u8)> {
        self.active_press.and_then(|active| active.physical_binding)
    }

    /// Only a new latch may use the current registration. An active latch with
    /// no mapped binding has frozen Unavailable support, not a missing default.
    pub fn observation_binding(&self, registration: Option<(u16, u8)>) -> Option<(u16, u8)> {
        match self.active_press {
            Some(active) => active.physical_binding,
            None => registration,
        }
    }

    /// Production/fake-state seam: select handle, sample whole chord and apply
    /// under one lock. A sample tagged with an old handle is always inert.
    /// Unavailable retains the ambiguous callback-only debt policy.
    pub fn release_observed(
        &mut self,
        handle: Option<PressHandle>,
        observation: PhysicalObservation,
    ) -> (Option<PressHandle>, ReleaseResult) {
        if handle != self.active_press() {
            return (handle, ReleaseResult::Stale);
        }
        match observation {
            PhysicalObservation::Down => (handle, ReleaseResult::Stale),
            PhysicalObservation::Up => (
                handle,
                handle.map_or(ReleaseResult::Stale, |h| self.release(h)),
            ),
            PhysicalObservation::Unavailable if self.active_press.is_some_and(|p| p.supported) => {
                // Losing observation support cannot downgrade a supported latch
                // to anonymous callback ownership. Its exact watcher still owns Up.
                (handle, ReleaseResult::Stale)
            }
            PhysicalObservation::Unavailable => self.os_released(),
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
        if active.interrupted {
            return ReleaseResult::Rearmed;
        }
        match active.mode {
            PhysicalHotkeyMode::Toggle => ReleaseResult::Rearmed,
            PhysicalHotkeyMode::Hold => {
                let token = HoldToken(handle);
                ReleaseResult::HoldEnded(GestureIntent::HoldEnded { token })
            }
        }
    }

    /// Callback-only fallback: debt protects one delayed callback per recovery.
    /// Missing callbacks and duplicate old callbacks remain indistinguishable;
    /// supported adapters must use release_observed even when debt is zero.
    pub fn os_released(&mut self) -> (Option<PressHandle>, ReleaseResult) {
        if self.recovered_release_debt > 0 {
            self.recovered_release_debt -= 1;
            return (None, ReleaseResult::Stale);
        }
        let Some(handle) = self.active_press.map(|active| active.handle) else {
            return (None, ReleaseResult::Stale);
        };
        (Some(handle), self.release(handle))
    }

    /// Recovery may only release the exact press it observed. A hold recovery
    /// emits the same semantic HoldEnded intent as the normal OS release path.
    pub fn physical_watcher_released(&mut self, handle: PressHandle) -> ReleaseResult {
        let supported = self
            .active_press
            .is_some_and(|p| p.handle == handle && p.supported);
        let result = self.release(handle);
        if result != ReleaseResult::Stale && !supported {
            self.recovered_release_debt = self.recovered_release_debt.saturating_add(1);
        }
        result
    }

    /// Stops semantic ownership; supported presses retain an Up rearm barrier.
    pub fn force_off(&mut self, reason: ForceOffReason) -> GestureIntent {
        let interrupted_hold = self.active_press.and_then(|active| {
            (active.mode == PhysicalHotkeyMode::Hold && !active.interrupted)
                .then_some(HoldToken(active.handle))
        });
        if let Some(active) = self.active_press.as_mut() {
            if active.supported {
                // One retained exact handle is a rearm barrier, not a live hold.
                // Its existing watcher exits at Up; repeats allocate no work.
                active.interrupted = true;
            } else {
                self.active_press = None;
                self.recovered_release_debt = self.recovered_release_debt.saturating_add(1);
            }
        }
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
    fn supported_release_matrix() {
        use PhysicalObservation::{Down, Up};
        for old_before_b in [false, true] {
            for watcher_first in [false, true] {
                for mode in [PhysicalHotkeyMode::Hold, PhysicalHotkeyMode::Toggle] {
                    let mut n = RecordingHotkeyGestureNormalizer::new();
                    // Callback-only A leaves anonymous debt, including missing A-up.
                    let a = accepted(n.press(PhysicalHotkeyMode::Hold));
                    n.force_off(ForceOffReason::Sleep);
                    if old_before_b {
                        n.os_released();
                    }
                    let b = accepted(n.press_observed(mode, Down));
                    assert_eq!(
                        n.press_observed(PhysicalHotkeyMode::Hold, Down),
                        PressResult::Duplicate
                    );
                    for _ in 0..3 {
                        assert_eq!(
                            n.release_observed(Some(b.handle), Down).1,
                            ReleaseResult::Stale
                        );
                    }
                    assert_eq!(
                        n.release_observed(Some(a.handle), Up).1,
                        ReleaseResult::Stale
                    );
                    let expected = match mode {
                        PhysicalHotkeyMode::Hold => {
                            ReleaseResult::HoldEnded(GestureIntent::HoldEnded {
                                token: HoldToken(b.handle),
                            })
                        }
                        PhysicalHotkeyMode::Toggle => ReleaseResult::Rearmed,
                    };
                    // No time input: even B shorter than a poll interval ends now.
                    // Watcher carries B; OS selects current under the same lock.
                    let first = if watcher_first {
                        Some(b.handle)
                    } else {
                        n.active_press()
                    };
                    assert_eq!(n.release_observed(first, Up).1, expected);
                    assert_eq!(
                        n.release_observed(Some(b.handle), Up).1,
                        ReleaseResult::Stale
                    );
                    assert_eq!(
                        n.release_observed(n.active_press(), Up).1,
                        ReleaseResult::Stale
                    );
                    let c = accepted(n.press_observed(mode, Down));
                    assert_eq!(n.physical_watcher_released(b.handle), ReleaseResult::Stale);
                    assert_eq!(
                        n.release_observed(Some(c.handle), Down).1,
                        ReleaseResult::Stale
                    );
                    assert_eq!(n.active_press(), Some(c.handle));
                }
            }
        }
    }

    #[test]
    fn supported_sleep_rearm_is_bounded_and_requires_observed_up() {
        use PhysicalObservation::{Down, Up};
        let mut n = RecordingHotkeyGestureNormalizer::new();
        for _ in 0..64 {
            let a = accepted(n.press_observed(PhysicalHotkeyMode::Hold, Down));
            for _ in 0..4 {
                n.force_off(ForceOffReason::Sleep);
                assert_eq!(n.active_press(), Some(a.handle));
                assert_eq!(
                    n.press_observed(PhysicalHotkeyMode::Toggle, Down),
                    PressResult::Duplicate
                );
                assert_eq!(
                    n.release_observed(Some(a.handle), Down).1,
                    ReleaseResult::Stale
                );
            }
            assert_eq!(
                n.release_observed(Some(a.handle), Up).1,
                ReleaseResult::Rearmed
            );
            assert_eq!(n.active_press(), None); // watcher terminates
            assert_eq!(n.recovered_release_debt, 0);
            let b = accepted(n.press_observed(PhysicalHotkeyMode::Toggle, Down));
            assert_eq!(
                n.release_observed(Some(a.handle), Up).1,
                ReleaseResult::Stale
            );
            assert_eq!(
                n.release_observed(Some(b.handle), Up).1,
                ReleaseResult::Rearmed
            );
            n.force_off(ForceOffReason::Sleep);
            n.force_off(ForceOffReason::Sleep);
            assert_eq!(n.active_press(), None);
        }
        let a = accepted(n.press_observed(PhysicalHotkeyMode::Hold, Down));
        n.force_off(ForceOffReason::Sleep);
        assert_eq!(
            n.press_observed(PhysicalHotkeyMode::Hold, Up),
            PressResult::Duplicate
        );
        assert_eq!(
            n.release_observed(Some(a.handle), Up).1,
            ReleaseResult::Stale
        );
        assert!(matches!(
            n.press_observed(PhysicalHotkeyMode::Hold, Down),
            PressResult::Accepted(_)
        ));
    }

    #[test]
    fn physical_binding_stays_with_handle_across_config_change_and_sleep() {
        let mut n = RecordingHotkeyGestureNormalizer::new();
        let a = accepted(n.press_observed(PhysicalHotkeyMode::Hold, PhysicalObservation::Down));
        n.bind_physical_chord(a.handle, (7, 3));
        n.bind_physical_chord(a.handle, (50, 0));
        n.force_off(ForceOffReason::Sleep);
        assert_eq!(n.physical_binding(), Some((7, 3)));
        n.release_observed(Some(a.handle), PhysicalObservation::Up);
        assert_eq!(n.physical_binding(), None);
        let b = accepted(n.press_observed(PhysicalHotkeyMode::Toggle, PhysicalObservation::Down));
        n.bind_physical_chord(a.handle, (7, 3));
        assert_eq!(n.physical_binding(), None);
        n.bind_physical_chord(b.handle, (50, 0));
        assert_eq!(n.physical_binding(), Some((50, 0)));
    }

    #[test]
    fn supported_missing_callback_and_temporary_unavailability() {
        use PhysicalObservation::{Down, Unavailable, Up};
        let mut n = RecordingHotkeyGestureNormalizer::new();
        let a = accepted(n.press_observed(PhysicalHotkeyMode::Hold, Down));
        n.force_off(ForceOffReason::Sleep);
        // No OS release for A: its token watcher observes Up and rearms.
        assert_eq!(
            n.release_observed(Some(a.handle), Up).1,
            ReleaseResult::Rearmed
        );
        let b = accepted(n.press_observed(PhysicalHotkeyMode::Hold, Down));
        assert_eq!(
            n.release_observed(Some(b.handle), Unavailable).1,
            ReleaseResult::Stale
        );
        assert_eq!(n.active_press(), Some(b.handle));
        // No OS release for B either. The same exact watcher boundary ends B.
        assert_eq!(
            n.release_observed(Some(b.handle), Up).1,
            ReleaseResult::HoldEnded(GestureIntent::HoldEnded {
                token: HoldToken(b.handle)
            })
        );
        assert_eq!(n.recovered_release_debt, 0);
        assert_eq!(
            n.release_observed(Some(a.handle), Up).1,
            ReleaseResult::Stale
        );
    }

    #[test]
    fn unavailable_cannot_distinguish_old_release_from_missing_a_and_b_up() {
        let mut n = RecordingHotkeyGestureNormalizer::new();
        accepted(n.press_observed(PhysicalHotkeyMode::Hold, PhysicalObservation::Unavailable));
        n.force_off(ForceOffReason::Sleep);
        let b =
            accepted(n.press_observed(PhysicalHotkeyMode::Hold, PhysicalObservation::Unavailable));
        assert_eq!(
            n.release_observed(Some(b.handle), PhysicalObservation::Unavailable),
            (None, ReleaseResult::Stale)
        );
        assert_eq!(n.active_press(), Some(b.handle)); // known fallback liveness limit
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
            ReleaseResult::Stale
        );
        assert_eq!(normalizer.active_press(), Some(current.handle));
        assert_eq!(
            normalizer.physical_watcher_released(current.handle),
            ReleaseResult::Rearmed
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
            ReleaseResult::Stale
        );
        assert_eq!(
            normalizer.physical_watcher_released(wrong_gesture),
            ReleaseResult::Stale
        );
        assert_eq!(normalizer.active_press(), Some(current.handle));
        assert_eq!(
            normalizer.physical_watcher_released(current.handle),
            ReleaseResult::Rearmed
        );
    }

    #[test]
    fn watcher_ends_interrupted_hold_with_the_original_token() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let hold = accepted(normalizer.press(PhysicalHotkeyMode::Hold));
        assert_eq!(
            normalizer.physical_watcher_released(hold.handle),
            ReleaseResult::HoldEnded(GestureIntent::HoldEnded {
                token: HoldToken(hold.handle)
            })
        );
        assert_eq!(normalizer.release(hold.handle), ReleaseResult::Stale);
    }

    #[test]
    fn delayed_os_release_after_watcher_cannot_release_a_new_hold() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let old = accepted(normalizer.press(PhysicalHotkeyMode::Hold));
        assert!(matches!(
            normalizer.physical_watcher_released(old.handle),
            ReleaseResult::HoldEnded(_)
        ));
        let current = accepted(normalizer.press(PhysicalHotkeyMode::Hold));

        assert_eq!(normalizer.os_released(), (None, ReleaseResult::Stale));
        assert_eq!(normalizer.active_press(), Some(current.handle));
        assert!(matches!(
            normalizer.physical_watcher_released(current.handle),
            ReleaseResult::HoldEnded(_)
        ));
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
    #[test]
    fn unavailable_latch_never_borrows_new_mapped_registration() {
        let mut normalizer = RecordingHotkeyGestureNormalizer::new();
        let a = accepted(
            normalizer.press_observed(PhysicalHotkeyMode::Hold, PhysicalObservation::Unavailable),
        );
        let mapped = Some((7, 3));
        assert_eq!(normalizer.observation_binding(mapped), None);
        assert_eq!(
            normalizer.press_observed(PhysicalHotkeyMode::Hold, PhysicalObservation::Unavailable,),
            PressResult::Duplicate
        );
        assert_eq!(normalizer.active_press(), Some(a.handle));
        assert_eq!(normalizer.observation_binding(mapped), None);
        normalizer.release_observed(Some(a.handle), PhysicalObservation::Unavailable);
        assert_eq!(normalizer.observation_binding(mapped), mapped);
    }
}
