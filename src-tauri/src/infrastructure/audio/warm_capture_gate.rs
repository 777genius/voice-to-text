//! Callback-boundary ownership for the opt-in macOS warm input.
//!
//! This module owns no PCM queue. `T` is the immutable episode payload
//! (processor and callback handles).
//! Times are supplied by the owner from one monotonic clock, allowing deterministic
//! freshness tests without sleeping or opening a microphone.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, TryLockError};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateError {
    Busy,
    Closed,
    Stale,
    Exhausted,
    Poisoned,
    DrainUncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LeaseIdentity {
    pub run_id: u64,
    pub capture_generation: u64,
    pub physical_generation: u64,
    pub epoch: u64,
}

struct DeliveryState {
    revoked: bool,
    in_flight: usize,
    terminal_claimed: bool,
}

pub(super) struct Lease<T> {
    pub identity: LeaseIdentity,
    pub payload: T,
    // The first callback after attach is omitted as an ambiguous boundary block.
    // This does not establish sample-exact acoustic privacy or a hardware bound.
    after_sequence: u64,
    delivery: Mutex<DeliveryState>,
    drained: Condvar,
}

pub(super) struct DeliveryPermit<T> {
    lease: Arc<Lease<T>>,
}

impl<T> Lease<T> {
    pub fn stop_complete(&self) -> bool {
        self.delivery
            .lock()
            .map(|state| state.revoked && state.in_flight == 0)
            .unwrap_or(false)
    }
    /// Must be obtained immediately before delivery, outside the owner lock.
    /// A snapshot alone never authorizes downstream enqueue.
    pub fn permit(self: &Arc<Self>) -> Result<Option<DeliveryPermit<T>>, GateError> {
        let mut state = match self.delivery.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => return Err(GateError::Busy),
            Err(TryLockError::Poisoned(_)) => return Err(GateError::Poisoned),
        };
        if state.revoked {
            return Ok(None);
        }
        state.in_flight = state.in_flight.checked_add(1).ok_or(GateError::Exhausted)?;
        Ok(Some(DeliveryPermit {
            lease: self.clone(),
        }))
    }

    fn revoke(&self) -> Result<(), GateError> {
        self.delivery
            .lock()
            .map_err(|_| GateError::Poisoned)?
            .revoked = true;
        Ok(())
    }

    /// Call only on the control side, never from a callback holding a permit.
    /// Timeout is not stop-complete: the owner must retain this lease.
    fn drain(&self, timeout: Duration) -> Result<(), GateError> {
        let state = self.delivery.lock().map_err(|_| GateError::Poisoned)?;
        let (state, _) = self
            .drained
            .wait_timeout_while(state, timeout, |s| s.in_flight != 0)
            .map_err(|_| GateError::Poisoned)?;
        if state.in_flight != 0 {
            Err(GateError::DrainUncertain)
        } else {
            Ok(())
        }
    }

    fn claim_terminal(&self) -> Result<bool, GateError> {
        let mut state = self.delivery.lock().map_err(|_| GateError::Poisoned)?;
        state.revoked = true;
        if state.terminal_claimed {
            return Ok(false);
        }
        state.terminal_claimed = true;
        Ok(true)
    }
}

impl<T> Drop for DeliveryPermit<T> {
    fn drop(&mut self) {
        // Recover only to account for an already-issued permit; poisoned state
        // remains poisoned, so future attach/delivery cannot report healthy.
        let mut state = self
            .lease
            .delivery
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.lease.drained.notify_all();
        }
    }
}

struct Physical {
    generation: u64,
    healthy: bool,
    last_callback: Option<Duration>,
    sequence: u64,
    period: Option<Duration>,
}

struct State<T> {
    next_physical: u64,
    next_epoch: u64,
    physical: Option<Physical>,
    lease: Option<Arc<Lease<T>>>,
}

pub(super) struct WarmCaptureGate<T> {
    state: Mutex<State<T>>,
    // Contended callbacks are discarded, never buffered for the next episode.
    // Non-zero healthy-run loss must block acceptance until investigated.
    pub contention_blocks: AtomicU64,
}

impl<T> Default for WarmCaptureGate<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                next_physical: 0,
                next_epoch: 0,
                physical: None,
                lease: None,
            }),
            contention_blocks: AtomicU64::new(0),
        }
    }
}

impl<T> WarmCaptureGate<T> {
    /// Two observed callbacks are necessary to establish a software freshness
    /// budget. This is liveness metadata, not acoustic timestamp qualification.
    pub fn freshness_budget(
        &self,
        generation: u64,
        now: Duration,
    ) -> Result<Option<Duration>, GateError> {
        let state = self.state.lock().map_err(|_| GateError::Poisoned)?;
        let Some(physical) = state.physical.as_ref() else {
            return Ok(None);
        };
        if physical.generation != generation || !physical.healthy || physical.sequence < 2 {
            return Ok(None);
        }
        let (Some(last), Some(period)) = (physical.last_callback, physical.period) else {
            return Ok(None);
        };
        let Some(period_budget) = period.checked_mul(3) else {
            return Ok(None);
        };
        let budget = Duration::from_millis(250).max(period_budget);
        if period.is_zero()
            || budget > Duration::from_secs(1)
            || now.checked_sub(last).map_or(true, |age| age > budget)
        {
            return Ok(None);
        }
        Ok(Some(budget))
    }
    pub fn snapshot(
        &self,
    ) -> Result<(Option<(u64, bool, Option<Duration>)>, Option<Arc<Lease<T>>>), GateError> {
        let state = self.state.lock().map_err(|_| GateError::Poisoned)?;
        Ok((
            state
                .physical
                .as_ref()
                .map(|p| (p.generation, p.healthy, p.last_callback)),
            state.lease.clone(),
        ))
    }

    pub fn reap_completed(&self) -> Result<(), GateError> {
        let mut state = self.state.lock().map_err(|_| GateError::Poisoned)?;
        if state
            .lease
            .as_ref()
            .is_some_and(|lease| lease.stop_complete())
        {
            state.lease = None;
        }
        Ok(())
    }
    /// Reserves one physical open. Failure/timeout does not release the slot:
    /// only the owning thread's actual drop acknowledgement does that.
    pub fn begin_open(&self) -> Result<u64, GateError> {
        let mut state = self.state.lock().map_err(|_| GateError::Poisoned)?;
        if state.physical.is_some() || state.lease.is_some() {
            return Err(GateError::Busy);
        }
        state.next_physical = state
            .next_physical
            .checked_add(1)
            .ok_or(GateError::Exhausted)?;
        let generation = state.next_physical;
        state.physical = Some(Physical {
            generation,
            healthy: true,
            last_callback: None,
            sequence: 0,
            period: None,
        });
        Ok(generation)
    }

    /// The typed native closure calls this BEFORE allocating/converting raw PCM.
    /// None means idle/stale/boundary input: return without touching processing.
    pub fn raw_callback(
        &self,
        generation: u64,
        now: Duration,
    ) -> Result<Option<Arc<Lease<T>>>, GateError> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => {
                self.contention_blocks.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
            Err(TryLockError::Poisoned(_)) => return Err(GateError::Poisoned),
        };
        let Some(physical) = state.physical.as_mut() else {
            return Ok(None);
        };
        if physical.generation != generation || !physical.healthy {
            return Ok(None);
        }
        physical.sequence = physical
            .sequence
            .checked_add(1)
            .ok_or(GateError::Exhausted)?;
        physical.period = physical
            .last_callback
            .and_then(|last| now.checked_sub(last));
        physical.last_callback = Some(now);
        let sequence = physical.sequence;
        let Some(lease) = state
            .lease
            .as_ref()
            .filter(|lease| sequence > lease.after_sequence)
        else {
            return Ok(None);
        };
        let delivery = match lease.delivery.try_lock() {
            Ok(delivery) => delivery,
            Err(TryLockError::WouldBlock) => {
                self.contention_blocks.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
            Err(TryLockError::Poisoned(_)) => return Err(GateError::Poisoned),
        };
        if delivery.revoked {
            return Ok(None);
        }
        Ok(Some(lease.clone()))
    }

    #[cfg(test)]
    pub fn attach(
        &self,
        run_id: u64,
        capture_generation: u64,
        payload: T,
        now: Duration,
        freshness: Duration,
    ) -> Result<Arc<Lease<T>>, GateError> {
        let physical_generation = self.snapshot()?.0.ok_or(GateError::Closed)?.0;
        self.attach_to(
            physical_generation,
            (run_id, capture_generation),
            payload,
            now,
            freshness,
        )
    }

    pub fn attach_to(
        &self,
        physical_generation: u64,
        identity: (u64, u64),
        payload: T,
        now: Duration,
        freshness: Duration,
    ) -> Result<Arc<Lease<T>>, GateError> {
        let mut state = self.state.lock().map_err(|_| GateError::Poisoned)?;
        if state.lease.is_some() {
            return Err(GateError::Busy);
        }
        let physical = state.physical.as_ref().ok_or(GateError::Closed)?;
        if physical.generation != physical_generation {
            return Err(GateError::Stale);
        }
        let last = physical.last_callback.ok_or(GateError::Stale)?;
        if !physical.healthy || now.checked_sub(last).ok_or(GateError::Stale)? > freshness {
            return Err(GateError::Stale);
        }
        let after_sequence = physical
            .sequence
            .checked_add(1)
            .ok_or(GateError::Exhausted)?;
        state.next_epoch = state
            .next_epoch
            .checked_add(1)
            .ok_or(GateError::Exhausted)?;
        let lease = Arc::new(Lease {
            identity: LeaseIdentity {
                run_id: identity.0,
                capture_generation: identity.1,
                physical_generation,
                epoch: state.next_epoch,
            },
            payload,
            after_sequence,
            delivery: Mutex::new(DeliveryState {
                revoked: false,
                in_flight: 0,
                terminal_claimed: false,
            }),
            drained: Condvar::new(),
        });
        state.lease = Some(lease.clone());
        Ok(lease)
    }

    pub fn release(&self, lease: &Arc<Lease<T>>, timeout: Duration) -> Result<(), GateError> {
        lease.revoke()?;
        // Never wait with the slot lock held. A terminal callback may need it.
        lease.drain(timeout)?;
        let mut state = self.state.lock().map_err(|_| GateError::Poisoned)?;
        if state
            .lease
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, lease))
        {
            state.lease = None;
        }
        Ok(())
    }

    /// Invalidation leaves physical ownership reserved until close_ack. Return
    /// the exact terminal recipient; invoke user code only AFTER this returns.
    pub fn invalidate(&self, generation: u64) -> Result<Option<Arc<Lease<T>>>, GateError> {
        self.invalidate_matching(generation, None)
    }

    pub fn invalidate_lease(
        &self,
        lease: &Arc<Lease<T>>,
    ) -> Result<Option<Arc<Lease<T>>>, GateError> {
        self.invalidate_matching(lease.identity.physical_generation, Some(lease))
    }

    fn invalidate_matching(
        &self,
        generation: u64,
        expected: Option<&Arc<Lease<T>>>,
    ) -> Result<Option<Arc<Lease<T>>>, GateError> {
        let mut state = self.state.lock().map_err(|_| GateError::Poisoned)?;
        if expected.is_some_and(|expected| {
            !state
                .lease
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected))
        }) {
            return Ok(None);
        }
        let Some(physical) = state.physical.as_mut() else {
            return Ok(None);
        };
        if physical.generation != generation || !physical.healthy {
            return Ok(None);
        }
        physical.healthy = false;
        let recipient = state.lease.clone();
        // This lock order is owner -> lease; release never holds lease -> owner.
        match recipient {
            Some(lease) if lease.claim_terminal()? => Ok(Some(lease)),
            _ => Ok(None),
        }
    }

    pub fn close_ack(&self, generation: u64) -> Result<(), GateError> {
        let mut state = self.state.lock().map_err(|_| GateError::Poisoned)?;
        if let Some(physical) = state.physical.as_ref() {
            if physical.generation == generation {
                if physical.healthy || state.lease.is_some() {
                    return Err(GateError::Busy);
                }
                state.physical = None;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    const NOW: Duration = Duration::from_secs(1);
    const FRESH: Duration = Duration::from_millis(250);

    fn ready<T>() -> (WarmCaptureGate<T>, u64) {
        let gate = WarmCaptureGate::default();
        let physical = gate.begin_open().unwrap();
        assert!(gate.raw_callback(physical, NOW).unwrap().is_none());
        (gate, physical)
    }

    #[test]
    fn idle_drops_before_processing_and_each_attach_omits_boundary_block() {
        let (gate, physical) = ready();
        for _ in 0..100 {
            assert!(gate.raw_callback(physical, NOW).unwrap().is_none());
        }
        let lease = gate.attach(1, 1, 42, NOW, FRESH).unwrap();
        assert!(gate.raw_callback(physical, NOW).unwrap().is_none());
        let snapshot = gate.raw_callback(physical, NOW).unwrap().unwrap();
        assert_eq!(snapshot.payload, 42);
        assert_eq!(snapshot.identity, lease.identity);
        assert_eq!(gate.contention_blocks.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn play_without_callback_and_stale_callbacks_cannot_admit() {
        let gate = WarmCaptureGate::<()>::default();
        let physical = gate.begin_open().unwrap();
        assert!(matches!(
            gate.attach(1, 1, (), NOW, FRESH),
            Err(GateError::Stale)
        ));
        gate.raw_callback(physical, NOW).unwrap();
        assert!(matches!(
            gate.attach(1, 1, (), NOW + FRESH + Duration::from_nanos(1), FRESH),
            Err(GateError::Stale)
        ));
        assert!(gate.attach(1, 1, (), NOW + FRESH, FRESH).is_ok());
    }

    #[test]
    fn revoke_before_permit_prevents_delivery_into_old_or_new_episode() {
        let (gate, physical) = ready();
        let a = gate.attach(7, 8, "A", NOW, FRESH).unwrap();
        gate.raw_callback(physical, NOW).unwrap();
        let snapshot_a = gate.raw_callback(physical, NOW).unwrap().unwrap();
        gate.release(&a, Duration::ZERO).unwrap();
        let b = gate.attach(7, 8, "B", NOW, FRESH).unwrap();
        assert_ne!(a.identity.epoch, b.identity.epoch);
        assert_eq!(
            a.identity.physical_generation,
            b.identity.physical_generation
        );
        assert!(snapshot_a.permit().unwrap().is_none());
        assert_eq!(snapshot_a.payload, "A");
        // A late repeated stop may not revoke B.
        gate.release(&a, Duration::ZERO).unwrap();
        assert!(b.permit().unwrap().is_some());
    }

    #[test]
    fn stop_after_permit_retains_owner_until_delivery_completes() {
        let (gate, physical) = ready();
        let a = gate.attach(1, 1, (), NOW, FRESH).unwrap();
        let permit = a.permit().unwrap().unwrap();
        assert_eq!(
            gate.release(&a, Duration::ZERO),
            Err(GateError::DrainUncertain)
        );
        assert!(matches!(
            gate.attach(2, 2, (), NOW, FRESH),
            Err(GateError::Busy)
        ));
        assert!(a.permit().unwrap().is_none());
        for _ in 0..100 {
            assert!(gate.raw_callback(physical, NOW).unwrap().is_none());
        }
        drop(permit);
        gate.release(&a, Duration::ZERO).unwrap();
        assert!(gate.attach(2, 2, (), NOW, FRESH).is_ok());
    }

    #[test]
    fn panic_releases_permit_and_terminal_is_claimed_once() {
        let (gate, physical) = ready();
        let a = gate.attach(1, 1, (), NOW, FRESH).unwrap();
        assert!(catch_unwind(AssertUnwindSafe(|| {
            let _permit = a.permit().unwrap().unwrap();
            panic!("injected downstream panic");
        }))
        .is_err());
        assert!(Arc::ptr_eq(
            &gate.invalidate(physical).unwrap().unwrap(),
            &a
        ));
        assert!(gate.invalidate(physical).unwrap().is_none());
        gate.release(&a, Duration::ZERO).unwrap();
        gate.close_ack(physical).unwrap();
    }

    #[test]
    fn invalidation_during_open_requires_actual_close_before_replacement() {
        let gate = WarmCaptureGate::<()>::default();
        let a = gate.begin_open().unwrap();
        assert!(gate.invalidate(a).unwrap().is_none());
        assert!(gate.raw_callback(a, NOW).unwrap().is_none());
        assert_eq!(gate.begin_open(), Err(GateError::Busy));
        gate.close_ack(a).unwrap();
        let b = gate.begin_open().unwrap();
        assert_ne!(a, b);
        assert!(gate.invalidate(a).unwrap().is_none());
        gate.close_ack(a).unwrap();
        gate.raw_callback(b, NOW).unwrap();
        assert!(gate.attach(1, 1, (), NOW, FRESH).is_ok());
    }

    #[test]
    fn terminal_recipient_remains_a_after_b_attaches() {
        let (gate, physical_a) = ready();
        let a = gate.attach(1, 1, "A", NOW, FRESH).unwrap();
        let error_a = gate.invalidate(physical_a).unwrap().unwrap();
        assert_eq!(gate.close_ack(physical_a), Err(GateError::Busy));
        gate.release(&a, Duration::ZERO).unwrap();
        gate.close_ack(physical_a).unwrap();
        let physical_b = gate.begin_open().unwrap();
        gate.raw_callback(physical_b, NOW).unwrap();
        let b = gate.attach(2, 2, "B", NOW, FRESH).unwrap();
        assert_eq!(error_a.payload, "A");
        assert!(b.permit().unwrap().is_some());
        assert!(gate.invalidate(physical_a).unwrap().is_none());
    }

    #[test]
    fn callback_contention_never_waits_or_buffers() {
        let (gate, physical) = ready::<()>();
        let _control = gate.state.lock().unwrap();
        assert!(gate.raw_callback(physical, NOW).unwrap().is_none());
        assert_eq!(gate.contention_blocks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn epochs_do_not_wrap_and_exhaustion_cannot_publish_a_lease() {
        let (gate, _) = ready::<()>();
        gate.state.lock().unwrap().next_epoch = u64::MAX;
        assert!(matches!(
            gate.attach(1, 1, (), NOW, FRESH),
            Err(GateError::Exhausted)
        ));
        assert!(gate.state.lock().unwrap().lease.is_none());
    }

    #[test]
    fn late_processing_failure_cannot_poison_same_physical_generation_new_lease() {
        let (gate, _) = ready();
        let a = gate.attach(1, 1, (), NOW, FRESH).unwrap();
        gate.release(&a, Duration::ZERO).unwrap();
        let b = gate.attach(2, 2, (), NOW, FRESH).unwrap();
        assert!(gate.invalidate_lease(&a).unwrap().is_none());
        assert!(b.permit().unwrap().is_some());
        assert!(gate.snapshot().unwrap().0.unwrap().1);
    }

    #[test]
    fn warm_freshness_needs_two_callbacks_and_bounded_observed_period() {
        let (gate, physical) = ready::<()>();
        assert_eq!(gate.freshness_budget(physical, NOW).unwrap(), None);
        let next = NOW + Duration::from_millis(10);
        gate.raw_callback(physical, next).unwrap();
        assert_eq!(
            gate.freshness_budget(physical, next).unwrap(),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            gate.freshness_budget(physical, next + Duration::from_millis(251))
                .unwrap(),
            None
        );
        let stalled = next + Duration::from_millis(400);
        gate.raw_callback(physical, stalled).unwrap();
        assert_eq!(gate.freshness_budget(physical, stalled).unwrap(), None);
        gate.raw_callback(physical, stalled + Duration::from_millis(20))
            .unwrap();
        assert_eq!(
            gate.freshness_budget(physical, stalled + Duration::from_millis(20))
                .unwrap(),
            Some(Duration::from_millis(250))
        );
    }
}
