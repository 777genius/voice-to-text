//! macOS dictation-only physical owner. Ordinary SystemAudioCapture keeps its
//! physical-stop semantics for translation, microphone tests and cold capture.

use super::warm_capture_gate::{GateError, Lease, WarmCaptureGate};
use super::warm_pcm::EpisodePcm;
use super::SystemAudioCapture;
use crate::domain::{
    AudioCapture, AudioCaptureErrorCallback, AudioCaptureIdentity, AudioChunk, AudioChunkCallback,
    AudioConfig, AudioError, AudioResult,
};
use async_trait::async_trait;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

const OPEN_DEADLINE: Duration = Duration::from_secs(2);
const ACTIVE_STALL: Duration = Duration::from_millis(2200);
const CONTROL_TICK: Duration = Duration::from_millis(50);
const DRAIN_DEADLINE: Duration = Duration::from_secs(2);

fn capture_error(message: impl Into<String>) -> AudioError {
    AudioError::Capture(message.into())
}
fn gate_error(error: GateError) -> AudioError {
    capture_error(format!("Warm input ownership: {error:?}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WarmInputFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub effective_name: String,
}

pub(crate) enum WarmRawBlock<'a> {
    I16(&'a [i16]),
    F32(&'a [f32]),
    U16(&'a [u16]),
}
pub(crate) type WarmRawCallback = Arc<dyn for<'a> Fn(WarmRawBlock<'a>) + Send + Sync>;

/// Narrow OS seam. The returned native handle never leaves the owner thread.
pub(crate) trait WarmNativeInput {
    fn format(&self) -> WarmInputFormat;
    fn play(&self) -> AudioResult<()>;
    /// (effective input is still valid, selected route still matches).
    /// A new preferred/default route is applied only at an episode boundary.
    fn validate_route(&self) -> AudioResult<(bool, bool)>;
}

pub(crate) trait WarmNativeFactory: Send + 'static {
    fn open(
        &mut self,
        raw: WarmRawCallback,
        error: AudioCaptureErrorCallback,
    ) -> AudioResult<Box<dyn WarmNativeInput>>;
}

struct Episode {
    processor: Mutex<EpisodePcm>,
    on_chunk: AudioChunkCallback,
    on_error: Option<AudioCaptureErrorCallback>,
}

struct Control {
    revision: u64,
    allowed: bool,
    desired: bool,
    shutdown: bool,
}
#[derive(Default)]
struct Observation {
    generation: Option<u64>,
    closed: bool,
    closed_revision: u64,
    ready: bool,
    validated_revision: u64,
    format: Option<WarmInputFormat>,
    error: Option<String>,
    failure_revision: Option<u64>,
}

#[derive(Clone, Debug)]
struct PreparationTicket {
    revision: u64,
    physical_generation: u64,
    format: WarmInputFormat,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct RawObservation {
    at: Duration,
    physical_generation: u64,
    lease_epoch: Option<u64>,
    interleaved_samples: usize,
    admitted: bool,
}

struct Shared {
    gate: WarmCaptureGate<Episode>,
    clock: Instant,
    control: Mutex<Control>,
    observation: Mutex<Observation>,
    changed: tokio::sync::Notify,
    wake: mpsc::SyncSender<()>,
    #[cfg(test)]
    before_open: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
    #[cfg(test)]
    raw_observer: Mutex<Option<Arc<dyn Fn(RawObservation) + Send + Sync>>>,
    #[cfg(test)]
    native_opens: std::sync::atomic::AtomicU64,
}

impl Shared {
    fn now(&self) -> Duration {
        self.clock.elapsed()
    }

    fn fail(&self, generation: u64, error: AudioError) {
        self.fail_matching(generation, None, error);
    }

    fn fail_matching(
        &self,
        generation: u64,
        expected: Option<&Arc<Lease<Episode>>>,
        error: AudioError,
    ) {
        let control = match self.control.lock() {
            Ok(control) => control,
            Err(_) => return,
        };
        let mut observation = match self.observation.lock() {
            Ok(observation) => observation,
            Err(_) => return,
        };
        if observation.generation != Some(generation) {
            return;
        }
        if !self
            .gate
            .snapshot()
            .ok()
            .and_then(|s| s.0)
            .is_some_and(|(current, healthy, _)| current == generation && healthy)
        {
            return;
        }
        let recipient = match expected {
            Some(lease) => match self.gate.invalidate_lease(lease) {
                Ok(Some(recipient)) => Some(recipient),
                _ => return,
            },
            None => self.gate.invalidate(generation).ok().flatten(),
        };
        // Failure metadata and revocation commit together before a newer
        // lifecycle request can reuse the now-invalid physical reservation.
        observation.ready = false;
        observation.error = Some(error.to_string());
        observation.failure_revision = Some(control.revision);
        drop(observation);
        drop(control);
        self.changed.notify_waiters();
        let _ = self.wake.try_send(());
        if let Some(lease) = recipient {
            if let Some(callback) = lease.payload.on_error.as_ref() {
                let _ = catch_unwind(AssertUnwindSafe(|| callback(error)));
            }
        }
    }

    fn raw(&self, generation: u64, raw: WarmRawBlock<'_>) {
        let admission = self.gate.raw_callback(generation, self.now());
        #[cfg(test)]
        if let Some(observer) = self
            .raw_observer
            .lock()
            .ok()
            .and_then(|observer| observer.clone())
        {
            let interleaved_samples = match &raw {
                WarmRawBlock::I16(pcm) => pcm.len(),
                WarmRawBlock::F32(pcm) => pcm.len(),
                WarmRawBlock::U16(pcm) => pcm.len(),
            };
            let lease_epoch = self
                .gate
                .snapshot()
                .ok()
                .and_then(|snapshot| snapshot.1)
                .map(|lease| lease.identity.epoch);
            observer(RawObservation {
                at: self.now(),
                physical_generation: generation,
                lease_epoch,
                interleaved_samples,
                admitted: matches!(&admission, Ok(Some(_))),
            });
        }
        let lease = match admission {
            Ok(Some(lease)) => lease,
            Ok(None) => return, // Before allocation, conversion, VAD, meter or storage.
            Err(error) => {
                self.fail(generation, gate_error(error));
                return;
            }
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| -> AudioResult<()> {
            let pcm = match raw {
                WarmRawBlock::I16(pcm) => pcm.to_vec(),
                WarmRawBlock::F32(pcm) => SystemAudioCapture::f32_to_i16(pcm),
                WarmRawBlock::U16(pcm) => SystemAudioCapture::u16_to_i16(pcm),
            };
            {
                let mut processor = match lease.payload.processor.try_lock() {
                    Ok(processor) => processor,
                    // CPAL serializes a stream's data callbacks. Reentrancy or
                    // contention here is a pipeline failure, not silent loss.
                    Err(TryLockError::WouldBlock) => {
                        return Err(capture_error("Concurrent warm PCM processing"))
                    }
                    Err(TryLockError::Poisoned(_)) => {
                        return Err(capture_error("Warm PCM processor poisoned"))
                    }
                };
                processor.ingest(&pcm)?;
            }
            loop {
                let samples = {
                    let mut processor = lease
                        .payload
                        .processor
                        .try_lock()
                        .map_err(|_| capture_error("Warm PCM processing unavailable"))?;
                    let Some(samples) = processor.next_chunk()? else {
                        return Ok(());
                    };
                    samples
                };
                let Some(_permit) = lease.permit().map_err(gate_error)? else {
                    return Ok(());
                };
                (lease.payload.on_chunk)(AudioChunk::new(samples, 16_000, 1));
            }
        }));
        match outcome {
            Ok(Ok(())) => {}
            failed => {
                let error = match failed {
                    Ok(Err(error)) => error,
                    _ => capture_error("Warm PCM callback panicked"),
                };
                self.fail_matching(generation, Some(&lease), error);
            }
        }
    }
}

pub(crate) struct WarmDictationInput {
    shared: Arc<Shared>,
}

impl WarmDictationInput {
    pub fn with_factory(factory: Box<dyn WarmNativeFactory>) -> AudioResult<Arc<Self>> {
        let (wake, receive) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            gate: WarmCaptureGate::default(),
            clock: Instant::now(),
            control: Mutex::new(Control {
                revision: 0,
                allowed: true,
                desired: false,
                shutdown: false,
            }),
            observation: Mutex::new(Observation {
                closed: true,
                ..Observation::default()
            }),
            changed: tokio::sync::Notify::new(),
            wake,
            #[cfg(test)]
            before_open: Mutex::new(None),
            #[cfg(test)]
            raw_observer: Mutex::new(None),
            #[cfg(test)]
            native_opens: std::sync::atomic::AtomicU64::new(0),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("dictation-input-owner".into())
            .spawn(move || owner_loop(worker, receive, factory))
            .map_err(|error| capture_error(format!("Cannot create input owner: {error}")))?;
        Ok(Arc::new(Self { shared }))
    }

    pub fn lease(self: &Arc<Self>) -> WarmDictationLease {
        WarmDictationLease {
            owner: self.clone(),
            lease: None,
            identity: None,
            on_error: None,
        }
    }

    pub fn is_warm_ready(&self) -> bool {
        self.shared
            .observation
            .lock()
            .map(|o| o.ready)
            .unwrap_or(false)
            && self
                .shared
                .gate
                .snapshot()
                .ok()
                .and_then(|s| s.0)
                .is_some_and(|(generation, _, _)| {
                    self.shared
                        .gate
                        .freshness_budget(generation, self.shared.now())
                        .ok()
                        .flatten()
                        .is_some()
                })
    }

    pub fn effective_format(&self) -> Option<WarmInputFormat> {
        self.shared
            .observation
            .lock()
            .ok()
            .and_then(|o| o.format.clone())
    }

    /// Immediate lifecycle barrier for sleep/logout/policy/mode transitions.
    /// Physical close acknowledgement remains asynchronous and mandatory.
    pub fn invalidate_now(&self) -> AudioResult<()> {
        self.request(false, false).map(|_| ())
    }

    /// Deny installed adapters the right to reopen after a lifecycle takeover.
    pub fn suspend_now(&self) -> AudioResult<()> {
        let mut control = self
            .shared
            .control
            .lock()
            .map_err(|_| capture_error("Input control poisoned"))?;
        if control.shutdown {
            return Err(capture_error("Input owner shut down"));
        }
        control.revision = control
            .revision
            .checked_add(1)
            .ok_or_else(|| capture_error("Input lifecycle exhausted"))?;
        control.allowed = false;
        control.desired = false;
        if let Some((generation, _, _)) = self.shared.gate.snapshot().map_err(gate_error)?.0 {
            self.shared
                .gate
                .invalidate(generation)
                .map_err(gate_error)?;
        }
        let mut observation = self
            .shared
            .observation
            .lock()
            .map_err(|_| capture_error("Input observation poisoned"))?;
        observation.ready = false;
        if observation.closed {
            observation.closed_revision = control.revision;
        }
        drop(observation);
        drop(control);
        let _ = self.shared.wake.try_send(());
        self.shared.changed.notify_waiters();
        Ok(())
    }

    /// Reallow a future explicit prewarm; never resume the previous recording.
    pub fn resume(&self) -> AudioResult<()> {
        let mut control = self
            .shared
            .control
            .lock()
            .map_err(|_| capture_error("Input control poisoned"))?;
        if control.shutdown {
            return Err(capture_error("Input owner shut down"));
        }
        if control.allowed {
            return Ok(());
        }
        control.revision = control
            .revision
            .checked_add(1)
            .ok_or_else(|| capture_error("Input lifecycle exhausted"))?;
        control.allowed = true;
        control.desired = false;
        drop(control);
        self.shared.changed.notify_waiters();
        Ok(())
    }

    fn request(&self, desired: bool, shutdown: bool) -> AudioResult<u64> {
        let mut control = self
            .shared
            .control
            .lock()
            .map_err(|_| capture_error("Input control poisoned"))?;
        if control.shutdown {
            if shutdown && !desired {
                return Ok(control.revision);
            }
            return Err(capture_error("Input owner shut down"));
        }
        if desired && !control.allowed {
            return Err(capture_error("Warm input suspended by lifecycle"));
        }
        control.revision = control
            .revision
            .checked_add(1)
            .ok_or_else(|| capture_error("Input lifecycle exhausted"))?;
        control.desired = desired;
        control.shutdown = shutdown;
        let revision = control.revision;
        if desired {
            if let Ok(mut observation) = self.shared.observation.lock() {
                observation.error = None;
            }
        }
        if !desired {
            // Invalidate synchronously, including during a blocked native open.
            if let Some((generation, _, _)) = self.shared.gate.snapshot().map_err(gate_error)?.0 {
                self.shared
                    .gate
                    .invalidate(generation)
                    .map_err(gate_error)?;
            }
            if let Ok(mut observation) = self.shared.observation.lock() {
                observation.ready = false;
                if observation.closed {
                    observation.closed_revision = revision;
                }
            }
        }
        drop(control);
        self.shared
            .wake
            .try_send(())
            .or_else(|error| match error {
                mpsc::TrySendError::Full(()) => Ok(()),
                error => Err(error),
            })
            .map_err(|_| capture_error("Input owner unavailable"))?;
        self.shared.changed.notify_waiters();
        Ok(revision)
    }

    pub(crate) fn shutdown_requested(&self) -> bool {
        self.shared
            .control
            .lock()
            .map(|control| control.shutdown)
            .unwrap_or(true)
    }

    pub async fn prewarm(&self) -> AudioResult<()> {
        self.prepare().await.map(|_| ())
    }

    async fn prepare(&self) -> AudioResult<PreparationTicket> {
        let revision = self.request(true, false)?;
        self.wait_prepared(revision).await
    }

    async fn wait_prepared(&self, revision: u64) -> AudioResult<PreparationTicket> {
        let result = tokio::time::timeout(OPEN_DEADLINE, async {
            loop {
                let changed = self.shared.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                {
                    let control = self
                        .shared
                        .control
                        .lock()
                        .map_err(|_| capture_error("Input control poisoned"))?;
                    if !control.allowed
                        || !control.desired
                        || control.shutdown
                        || control.revision != revision
                    {
                        return Err(capture_error("Warm preparation superseded"));
                    }
                    let observation = self
                        .shared
                        .observation
                        .lock()
                        .map_err(|_| capture_error("Input observation poisoned"))?;
                    if let Some(error) = observation
                        .error
                        .as_ref()
                        .filter(|_| observation.failure_revision == Some(revision))
                    {
                        return Err(capture_error(error.clone()));
                    }
                    if observation.ready && observation.validated_revision == revision {
                        let physical_generation = observation.generation.ok_or_else(|| {
                            capture_error("Ready input lacks physical generation")
                        })?;
                        let format = observation
                            .format
                            .clone()
                            .ok_or_else(|| capture_error("Ready input lacks native format"))?;
                        return Ok(PreparationTicket {
                            revision,
                            physical_generation,
                            format,
                        });
                    }
                }
                changed.await;
            }
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => {
                // Never allow a timed-out open to publish readiness later.
                if let Ok(mut control) = self.shared.control.lock() {
                    // An old timeout must not cancel a newer lifecycle request.
                    if control.revision == revision {
                        control.desired = false;
                        if let Ok((Some((generation, _, _)), _)) = self.shared.gate.snapshot() {
                            let _ = self.shared.gate.invalidate(generation);
                        }
                        if let Ok(mut observation) = self.shared.observation.lock() {
                            observation.ready = false;
                        }
                        let _ = self.shared.wake.try_send(());
                    }
                }
                Err(capture_error(
                    "Warm input opening timed out; close acknowledgement required",
                ))
            }
        }
    }

    pub async fn close(&self) -> AudioResult<()> {
        let revision = self.request(false, false)?;
        self.wait_closed(revision).await
    }

    pub async fn shutdown(&self) -> AudioResult<()> {
        let revision = self.request(false, true)?;
        self.wait_closed(revision).await
    }

    async fn wait_closed(&self, revision: u64) -> AudioResult<()> {
        tokio::time::timeout(DRAIN_DEADLINE, async {
            loop {
                let changed = self.shared.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                {
                    let control = self
                        .shared
                        .control
                        .lock()
                        .map_err(|_| capture_error("Input control poisoned"))?;
                    if control.revision != revision || control.desired {
                        return Err(capture_error(
                            "Warm close superseded by a newer lifecycle request",
                        ));
                    }
                    let observation = self
                        .shared
                        .observation
                        .lock()
                        .map_err(|_| capture_error("Input observation poisoned"))?;
                    if observation.closed && observation.closed_revision >= revision {
                        return Ok(());
                    }
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| capture_error("Warm input close uncertain; replacement forbidden"))?
    }

    fn attach_prepared(
        &self,
        ticket: PreparationTicket,
        identity: AudioCaptureIdentity,
        episode: Episode,
    ) -> AudioResult<Arc<Lease<Episode>>> {
        // This control guard also serializes open reservation and lifecycle
        // revoke. A ticket can never attach to a replacement physical stream.
        let control = self
            .shared
            .control
            .lock()
            .map_err(|_| capture_error("Input control poisoned"))?;
        if !control.allowed
            || !control.desired
            || control.shutdown
            || control.revision != ticket.revision
        {
            return Err(capture_error("Warm preparation ticket superseded"));
        }
        let observation = self
            .shared
            .observation
            .lock()
            .map_err(|_| capture_error("Input observation poisoned"))?;
        if !observation.ready
            || observation.validated_revision != ticket.revision
            || observation.generation != Some(ticket.physical_generation)
            || observation.format.as_ref() != Some(&ticket.format)
        {
            return Err(capture_error(
                "Warm preparation ticket no longer matches native input",
            ));
        }
        let freshness = self
            .shared
            .gate
            .freshness_budget(ticket.physical_generation, self.shared.now())
            .map_err(gate_error)?
            .ok_or_else(|| capture_error("Warm native callback freshness expired"))?;
        self.shared
            .gate
            .attach_to(
                ticket.physical_generation,
                (identity.run_id, identity.generation),
                episode,
                self.shared.now(),
                freshness,
            )
            .map_err(gate_error)
    }
}

impl Drop for WarmDictationInput {
    fn drop(&mut self) {
        let _ = self.request(false, true);
    }
}

fn owner_loop(
    shared: Arc<Shared>,
    receive: mpsc::Receiver<()>,
    mut factory: Box<dyn WarmNativeFactory>,
) {
    let mut input: Option<Box<dyn WarmNativeInput>> = None;
    let mut generation = None;
    let mut opened = Instant::now();
    let mut failed_revision = None;
    let mut last_route_check = Instant::now();
    let mut last_route_revision = None;
    let mut route_change_pending = false;
    loop {
        let (revision, desired, shutdown) = match shared.control.lock() {
            Ok(control) => (control.revision, control.desired, control.shutdown),
            Err(_) => return,
        };
        let _ = shared.gate.reap_completed();
        let snapshot = match shared.gate.snapshot() {
            Ok(snapshot) => snapshot,
            Err(_) => return,
        };
        let healthy = snapshot.0.is_some_and(|(_, healthy, _)| healthy);
        if desired
            && healthy
            && (last_route_revision != Some(revision)
                || last_route_check.elapsed() >= Duration::from_millis(650))
        {
            last_route_check = Instant::now();
            last_route_revision = Some(revision);
            if let Some(native) = input.as_ref() {
                match native.validate_route() {
                    Ok((valid, matches)) => {
                        route_change_pending = !matches;
                        if !valid {
                            shared.fail(
                                generation.unwrap(),
                                capture_error("Effective microphone changed or disappeared"),
                            );
                        }
                    }
                    Err(error) => shared.fail(generation.unwrap(), error),
                }
            }
        }
        if desired && route_change_pending {
            if let Some(current) = generation {
                match shared.gate.invalidate_if_idle(current) {
                    Ok(true) => route_change_pending = false,
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
        }
        let (healthy, active) = match shared.gate.snapshot() {
            Ok((physical, lease)) => (
                physical.is_some_and(|(_, healthy, _)| healthy),
                lease.is_some(),
            ),
            Err(_) => return,
        };
        if generation.is_some() && (!desired || !healthy) {
            let current = generation.unwrap();
            let _ = shared.gate.invalidate(current);
            // Native handle construction/play/drop all happen on this thread.
            drop(input.take());
            if shared.gate.close_ack(current).is_ok() {
                generation = None;
                route_change_pending = false;
                let control = shared.control.lock().unwrap();
                let mut observation = shared.observation.lock().unwrap();
                observation.closed = true;
                if !control.desired {
                    observation.closed_revision = control.revision;
                }
                observation.ready = false;
                observation.generation = None;
                observation.format = None;
                shared.changed.notify_waiters();
            }
            if !healthy {
                failed_revision = shared
                    .observation
                    .lock()
                    .ok()
                    .and_then(|o| o.failure_revision);
            }
        }
        if shutdown && generation.is_none() {
            return;
        }
        if desired && generation.is_none() && failed_revision != Some(revision) {
            #[cfg(test)]
            if let Some((entered, release)) = shared.before_open.lock().unwrap().take() {
                entered.send(()).unwrap();
                release.recv().unwrap();
            }
            // Reserve open and withdraw the closed acknowledgement atomically
            // with lifecycle requests. Otherwise close() could observe Closed,
            // return success, and this stale iteration could open afterward.
            let control = match shared.control.lock() {
                Ok(control) => control,
                Err(_) => return,
            };
            if !control.allowed
                || !control.desired
                || control.shutdown
                || control.revision != revision
            {
                continue;
            }
            let current = match shared.gate.begin_open() {
                Ok(generation) => generation,
                Err(_) => continue,
            };
            generation = Some(current);
            {
                let mut observation = shared.observation.lock().unwrap();
                observation.closed = false;
                observation.generation = Some(current);
                observation.ready = false;
                observation.error = None;
                observation.failure_revision = None;
                observation.format = None;
            }
            drop(control);
            let raw_shared = shared.clone();
            let error_shared = shared.clone();
            match factory.open(
                Arc::new(move |raw| raw_shared.raw(current, raw)),
                Arc::new(move |error| error_shared.fail(current, error)),
            ) {
                Ok(native) => {
                    #[cfg(test)]
                    shared
                        .native_opens
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let format = native.format();
                    input = Some(native);
                    opened = Instant::now();
                    let still_wanted = shared
                        .control
                        .lock()
                        .map(|c| c.allowed && c.desired && !c.shutdown && c.revision == revision)
                        .unwrap_or(false);
                    if !still_wanted {
                        let _ = shared.gate.invalidate(current);
                    } else if let Err(error) = input.as_ref().unwrap().play() {
                        shared.fail(current, error);
                    } else {
                        shared.observation.lock().unwrap().format = Some(format);
                    }
                }
                Err(error) => shared.fail(current, error),
            }
        }
        if let Some((current, healthy, last)) = shared.gate.snapshot().ok().and_then(|s| s.0) {
            if healthy {
                if last.is_none() && opened.elapsed() >= OPEN_DEADLINE {
                    shared.fail(
                        current,
                        capture_error("Microphone produced no native callback"),
                    );
                } else if active
                    && last.is_some_and(|last| shared.now().saturating_sub(last) >= ACTIVE_STALL)
                {
                    shared.fail(current, capture_error("Native microphone callback stalled"));
                } else {
                    let mut observation = shared.observation.lock().unwrap();
                    observation.ready = shared
                        .gate
                        .freshness_budget(current, shared.now())
                        .ok()
                        .flatten()
                        .is_some();
                    observation.validated_revision = revision;
                    shared.changed.notify_waiters();
                }
            }
        }
        let _ = receive.recv_timeout(CONTROL_TICK);
    }
}

pub(crate) struct WarmDictationLease {
    owner: Arc<WarmDictationInput>,
    lease: Option<Arc<Lease<Episode>>>,
    identity: Option<AudioCaptureIdentity>,
    on_error: Option<AudioCaptureErrorCallback>,
}

impl WarmDictationLease {
    pub fn device_name(&self) -> Option<String> {
        self.owner
            .effective_format()
            .map(|format| format.effective_name)
    }
}

#[async_trait]
impl AudioCapture for WarmDictationLease {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        if config.sample_rate != 16_000 || config.channels != 1 {
            return Err(capture_error("Warm dictation requires 16 kHz mono"));
        }
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if self.is_capturing() {
            return Err(capture_error("Warm lease already active or draining"));
        }
        let identity = self
            .identity
            .ok_or_else(|| capture_error("Warm capture requires logical identity"))?;
        let ticket = self.owner.prepare().await?;
        let episode = Episode {
            processor: Mutex::new(EpisodePcm::new(
                ticket.format.sample_rate,
                ticket.format.channels,
            )?),
            on_chunk,
            on_error: self.on_error.clone(),
        };
        self.lease = Some(self.owner.attach_prepared(ticket, identity, episode)?);
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        if let Some(lease) = self.lease.as_ref() {
            // Revoke synchronously; no callback can obtain a new permit while
            // spawn_blocking waits for previously accepted work to finish.
            let immediate = self.owner.shared.gate.release(lease, Duration::ZERO);
            if immediate == Err(GateError::DrainUncertain) {
                let shared = self.owner.shared.clone();
                let lease = lease.clone();
                tokio::task::spawn_blocking(move || shared.gate.release(&lease, DRAIN_DEADLINE))
                    .await
                    .map_err(|_| capture_error("Warm lease drain task failed"))?
                    .map_err(gate_error)?;
            } else {
                immediate.map_err(gate_error)?;
            }
            self.lease = None;
            let _ = self.owner.shared.wake.try_send(());
        }
        Ok(())
    }

    fn set_capture_identity(&mut self, identity: Option<AudioCaptureIdentity>) {
        self.identity = identity;
    }
    fn set_terminal_error_callback(&mut self, callback: Option<AudioCaptureErrorCallback>) {
        self.on_error = callback;
    }
    fn is_capturing(&self) -> bool {
        self.lease
            .as_ref()
            .is_some_and(|lease| !lease.stop_complete())
    }
    fn config(&self) -> AudioConfig {
        AudioConfig::default()
    }
}

impl Drop for WarmDictationLease {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.as_ref() {
            let _ = self.owner.shared.gate.release(lease, Duration::ZERO);
            let _ = self.owner.shared.wake.try_send(());
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "warm_native_acceptance.rs"]
mod native_acceptance;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Source {
        callbacks: Mutex<Vec<(WarmRawCallback, AudioCaptureErrorCallback)>>,
        opens: AtomicUsize,
        closes: AtomicUsize,
        route_valid: std::sync::atomic::AtomicBool,
        route_matches: std::sync::atomic::AtomicBool,
        route_barrier: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
        open_barrier: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
        sample_rate: AtomicUsize,
    }
    struct FakeFactory(Arc<Source>);
    struct FakeInput(Arc<Source>, WarmRawCallback, std::thread::ThreadId);

    impl WarmNativeFactory for FakeFactory {
        fn open(
            &mut self,
            raw: WarmRawCallback,
            error: AudioCaptureErrorCallback,
        ) -> AudioResult<Box<dyn WarmNativeInput>> {
            self.0.opens.fetch_add(1, Ordering::SeqCst);
            if let Some((entered, release)) = self.0.open_barrier.lock().unwrap().take() {
                entered.send(()).unwrap();
                release.recv().unwrap();
            }
            self.0.callbacks.lock().unwrap().push((raw.clone(), error));
            Ok(Box::new(FakeInput(
                self.0.clone(),
                raw,
                std::thread::current().id(),
            )))
        }
    }
    impl WarmNativeInput for FakeInput {
        fn format(&self) -> WarmInputFormat {
            WarmInputFormat {
                sample_rate: self.0.sample_rate.load(Ordering::SeqCst) as u32,
                channels: 1,
                effective_name: "fake native".into(),
            }
        }
        fn play(&self) -> AudioResult<()> {
            assert_eq!(self.2, std::thread::current().id());
            (self.1)(WarmRawBlock::I16(&[0; 1023]));
            (self.1)(WarmRawBlock::I16(&[0; 1023]));
            Ok(())
        }
        fn validate_route(&self) -> AudioResult<(bool, bool)> {
            if let Some((entered, release)) = self.0.route_barrier.lock().unwrap().take() {
                entered.send(()).unwrap();
                release.recv().unwrap();
            }
            Ok((
                self.0.route_valid.load(Ordering::SeqCst),
                self.0.route_matches.load(Ordering::SeqCst),
            ))
        }
    }
    impl Drop for FakeInput {
        fn drop(&mut self) {
            assert_eq!(self.2, std::thread::current().id());
            self.0.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn setup() -> (Arc<Source>, Arc<WarmDictationInput>) {
        let source = Arc::new(Source::default());
        source.route_valid.store(true, Ordering::SeqCst);
        source.route_matches.store(true, Ordering::SeqCst);
        source.sample_rate.store(48_000, Ordering::SeqCst);
        let owner =
            WarmDictationInput::with_factory(Box::new(FakeFactory(source.clone()))).unwrap();
        (source, owner)
    }
    fn raw(source: &Source, input: &[i16]) {
        let callback = source.callbacks.lock().unwrap().last().unwrap().0.clone();
        callback(WarmRawBlock::I16(input));
    }

    #[tokio::test]
    async fn one_physical_input_ten_episodes_no_idle_pcm_and_fresh_processing() {
        let (source, owner) = setup();
        owner.prewarm().await.unwrap();
        assert!(owner.is_warm_ready());
        let received = Arc::new(Mutex::new(Vec::new()));
        for run in 1..=10 {
            let mut lease = owner.lease();
            lease.set_capture_identity(Some(AudioCaptureIdentity {
                run_id: run,
                generation: run,
            }));
            let output = received.clone();
            lease
                .start_capture(Arc::new(move |chunk| {
                    output.lock().unwrap().extend(chunk.data)
                }))
                .await
                .unwrap();
            raw(&source, &[9999; 1023]); // Explicit boundary omission, not FIFO loss.
            raw(&source, &vec![run as i16; 1023]);
            raw(&source, &[7777; 100]); // Unaccepted residual must not reach next run.
            lease.stop_capture().await.unwrap();
            assert!(!lease.is_capturing());
            for _ in 0..100 {
                raw(&source, &[8888; 1023]);
            }
        }
        let samples = received.lock().unwrap();
        assert_eq!(samples.len(), 10 * 341);
        for (index, chunk) in samples.chunks_exact(341).enumerate() {
            assert!(chunk.iter().all(|sample| *sample == index as i16 + 1));
        }
        drop(samples);
        assert_eq!(source.opens.load(Ordering::SeqCst), 1);
        owner.close().await.unwrap();
        assert_eq!(source.closes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn physical_failure_routes_once_and_late_error_cannot_poison_replacement() {
        let (source, owner) = setup();
        let mut lease = owner.lease();
        lease.set_capture_identity(Some(AudioCaptureIdentity {
            run_id: 1,
            generation: 1,
        }));
        let errors = Arc::new(AtomicUsize::new(0));
        let error_count = errors.clone();
        lease.set_terminal_error_callback(Some(Arc::new(move |_| {
            error_count.fetch_add(1, Ordering::SeqCst);
        })));
        lease.start_capture(Arc::new(|_| {})).await.unwrap();
        let old_error = source.callbacks.lock().unwrap()[0].1.clone();
        old_error(capture_error("injected disconnect"));
        old_error(capture_error("duplicate disconnect"));
        assert_eq!(errors.load(Ordering::SeqCst), 1);
        lease.stop_capture().await.unwrap();
        // The next user intent may arrive before the control thread has closed
        // the failed input. It must wait for actual close, then recover once.
        owner.prewarm().await.unwrap();
        old_error(capture_error("late disconnect"));
        assert!(owner.is_warm_ready());
        assert_eq!(source.opens.load(Ordering::SeqCst), 2);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn same_public_identity_gets_new_epoch_and_downstream_panic_is_terminal() {
        let (source, owner) = setup();
        let mut lease = owner.lease();
        lease.set_capture_identity(Some(AudioCaptureIdentity {
            run_id: 1,
            generation: 1,
        }));
        lease.start_capture(Arc::new(|_| {})).await.unwrap();
        let first = lease.lease.as_ref().unwrap().identity;
        lease.stop_capture().await.unwrap();
        let errors = Arc::new(AtomicUsize::new(0));
        let count = errors.clone();
        lease.set_terminal_error_callback(Some(Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        })));
        lease
            .start_capture(Arc::new(|_| panic!("injected downstream panic")))
            .await
            .unwrap();
        assert_ne!(first.epoch, lease.lease.as_ref().unwrap().identity.epoch);
        raw(&source, &[0; 1023]);
        raw(&source, &[1; 1023]);
        assert_eq!(errors.load(Ordering::SeqCst), 1);
        assert!(!owner.is_warm_ready());
        lease.stop_capture().await.unwrap();
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn typed_raw_formats_use_same_processor_and_lifecycle_revokes_idle_and_active() {
        let (source, owner) = setup();
        for (run, expected) in [(1, 16383), (2, 1234)] {
            let mut lease = owner.lease();
            lease.set_capture_identity(Some(AudioCaptureIdentity {
                run_id: run,
                generation: run,
            }));
            let received = Arc::new(Mutex::new(Vec::new()));
            let output = received.clone();
            lease
                .start_capture(Arc::new(move |chunk| {
                    output.lock().unwrap().extend(chunk.data)
                }))
                .await
                .unwrap();
            raw(&source, &[0; 1023]);
            let callback = source.callbacks.lock().unwrap().last().unwrap().0.clone();
            if run == 1 {
                callback(WarmRawBlock::F32(&[0.5; 1023]));
            } else {
                callback(WarmRawBlock::U16(&[34002; 1023]));
            }
            assert_eq!(*received.lock().unwrap(), vec![expected; 341]);
            owner.invalidate_now().unwrap();
            raw(&source, &[9999; 1023]);
            assert_eq!(received.lock().unwrap().len(), 341);
            lease.stop_capture().await.unwrap();
            owner.close().await.unwrap();
        }
        assert_eq!(source.opens.load(Ordering::SeqCst), 2);
        owner.shutdown().await.unwrap();
        assert!(owner.prewarm().await.is_err());
    }

    #[tokio::test]
    async fn close_during_blocked_open_disposes_late_handle_without_play_or_revival() {
        let (source, owner) = setup();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *source.open_barrier.lock().unwrap() = Some((entered_tx, release_rx));
        let preparing = owner.clone();
        let task = tokio::spawn(async move { preparing.prewarm().await });
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        owner.invalidate_now().unwrap();
        assert!(!owner.is_warm_ready());
        assert_eq!(source.opens.load(Ordering::SeqCst), 1);
        assert_eq!(source.closes.load(Ordering::SeqCst), 0);
        release_tx.send(()).unwrap();
        assert!(task.await.unwrap().is_err());
        owner.close().await.unwrap();
        assert_eq!(source.opens.load(Ordering::SeqCst), 1);
        assert_eq!(source.closes.load(Ordering::SeqCst), 1);
        assert!(!owner.is_warm_ready());
    }

    #[tokio::test]
    async fn acknowledged_close_cannot_be_followed_by_stale_open_reservation() {
        let (source, owner) = setup();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *owner.shared.before_open.lock().unwrap() = Some((entered_tx, release_rx));
        let preparing = owner.clone();
        let old = tokio::spawn(async move { preparing.prewarm().await });
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        // The worker has decided to open but has not reserved native ownership.
        // Close is allowed to acknowledge here only if that decision is fenced.
        owner.close().await.unwrap();
        assert!(old.await.unwrap().is_err());
        assert_eq!(source.opens.load(Ordering::SeqCst), 0);
        release_tx.send(()).unwrap();
        owner.prewarm().await.unwrap();
        assert_eq!(source.opens.load(Ordering::SeqCst), 1);
        owner.close().await.unwrap();
        assert_eq!(source.closes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn uncertain_shutdown_is_idempotent_until_physical_close_is_acknowledged() {
        let (source, owner) = setup();
        owner.prewarm().await.unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *source.route_barrier.lock().unwrap() = Some((entered_tx, release_rx));
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();

        let error = owner.shutdown().await.unwrap_err();
        assert!(error.to_string().contains("replacement forbidden"));
        assert!(owner.shutdown_requested());
        assert_eq!(source.closes.load(Ordering::SeqCst), 0);

        release_tx.send(()).unwrap();
        owner.shutdown().await.unwrap();
        assert_eq!(source.closes.load(Ordering::SeqCst), 1);
        assert!(owner.prewarm().await.is_err());
    }

    #[tokio::test]
    async fn cancelled_open_l1_does_not_suppress_l3_and_l2_cannot_claim_new_close() {
        let (source, owner) = setup();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *source.open_barrier.lock().unwrap() = Some((entered_tx, release_rx));
        let preparing = owner.clone();
        let l1 = tokio::spawn(async move { preparing.prewarm().await });
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        let l2 = owner.request(false, false).unwrap();
        let l3 = owner.request(true, false).unwrap();
        assert!(owner.wait_closed(l2).await.is_err());
        assert_eq!(source.opens.load(Ordering::SeqCst), 1);
        assert_eq!(source.closes.load(Ordering::SeqCst), 0);
        release_tx.send(()).unwrap();
        let ticket = owner.wait_prepared(l3).await.unwrap();
        assert_eq!(ticket.revision, l3);
        assert!(l1.await.unwrap().is_err());
        assert_eq!(source.opens.load(Ordering::SeqCst), 2);
        assert_eq!(source.closes.load(Ordering::SeqCst), 1);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn preparation_ticket_cannot_attach_after_revalidation_or_replacement() {
        let (source, owner) = setup();
        let a = owner.prepare().await.unwrap();
        let same_physical_new_revision = owner.prepare().await.unwrap();
        let episode = |format: &WarmInputFormat| Episode {
            processor: Mutex::new(EpisodePcm::new(format.sample_rate, format.channels).unwrap()),
            on_chunk: Arc::new(|_| {}),
            on_error: None,
        };
        let identity = AudioCaptureIdentity {
            run_id: 1,
            generation: 1,
        };
        assert!(owner
            .attach_prepared(a.clone(), identity, episode(&a.format))
            .is_err());
        assert_eq!(
            a.physical_generation,
            same_physical_new_revision.physical_generation
        );
        owner.close().await.unwrap();
        source.sample_rate.store(44_100, Ordering::SeqCst);
        let b = owner.prepare().await.unwrap();
        assert_ne!(a.physical_generation, b.physical_generation);
        assert_ne!(a.format, b.format);
        assert!(owner
            .attach_prepared(a.clone(), identity, episode(&a.format))
            .is_err());
        let mut wrong_physical = b.clone();
        wrong_physical.physical_generation = a.physical_generation;
        assert!(owner
            .attach_prepared(wrong_physical, identity, episode(&b.format))
            .is_err());
        let mut wrong_format = b.clone();
        wrong_format.format = a.format;
        assert!(owner
            .attach_prepared(wrong_format, identity, episode(&b.format))
            .is_err());
        assert!(owner.shared.gate.snapshot().unwrap().1.is_none());
        let lease = owner
            .attach_prepared(b.clone(), identity, episode(&b.format))
            .unwrap();
        owner.shared.gate.release(&lease, Duration::ZERO).unwrap();
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn route_change_waits_for_attach_racing_validation() {
        let (source, owner) = setup();
        let ticket = owner.prepare().await.unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *source.route_barrier.lock().unwrap() = Some((entered_tx, release_rx));
        source.route_matches.store(false, Ordering::SeqCst);

        let callback_source = source.clone();
        let callbacks = tokio::spawn(async move {
            for _ in 0..100 {
                raw(&callback_source, &[0; 1023]);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let entered = tokio::task::spawn_blocking(move || entered_rx.recv().unwrap());
        entered.await.unwrap();
        let lease = owner
            .attach_prepared(
                ticket.clone(),
                AudioCaptureIdentity {
                    run_id: 1,
                    generation: 1,
                },
                Episode {
                    processor: Mutex::new(
                        EpisodePcm::new(ticket.format.sample_rate, ticket.format.channels).unwrap(),
                    ),
                    on_chunk: Arc::new(|_| {}),
                    on_error: None,
                },
            )
            .unwrap();
        release_tx.send(()).unwrap();
        callbacks.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(source.closes.load(Ordering::SeqCst), 0);
        assert!(owner.shared.gate.snapshot().unwrap().0.unwrap().1);
        assert!(lease.permit().unwrap().is_some());

        source.route_matches.store(true, Ordering::SeqCst);
        owner.shared.gate.release(&lease, Duration::ZERO).unwrap();
        for _ in 0..100 {
            if source.opens.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(source.closes.load(Ordering::SeqCst), 1);
        assert_eq!(source.opens.load(Ordering::SeqCst), 2);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn suspension_fences_open_completion_and_resume_requires_explicit_prepare() {
        let (source, owner) = setup();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *source.open_barrier.lock().unwrap() = Some((entered_tx, release_rx));
        let preparing = owner.clone();
        let task = tokio::spawn(async move { preparing.prewarm().await });
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        owner.suspend_now().unwrap();
        assert!(owner.prewarm().await.is_err());
        assert!(!owner.shared.control.lock().unwrap().allowed);
        release_tx.send(()).unwrap();
        assert!(task.await.unwrap().is_err());
        owner.close().await.unwrap();
        owner.resume().unwrap();
        assert!(!owner.shared.control.lock().unwrap().desired);
        raw(&source, &[7777; 1023]); // Late callback from the cancelled opening.
        assert!(!owner.is_warm_ready());
        assert_eq!(source.opens.load(Ordering::SeqCst), 1);
        owner.prewarm().await.unwrap();
        assert_eq!(source.opens.load(Ordering::SeqCst), 2);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn suspension_revokes_active_lease_and_installed_adapter_cannot_reopen() {
        let (source, owner) = setup();
        let mut lease = owner.lease();
        lease.set_capture_identity(Some(AudioCaptureIdentity {
            run_id: 1,
            generation: 1,
        }));
        let received = Arc::new(AtomicUsize::new(0));
        let count = received.clone();
        lease
            .start_capture(Arc::new(move |chunk| {
                count.fetch_add(chunk.data.len(), Ordering::SeqCst);
            }))
            .await
            .unwrap();
        raw(&source, &[0; 1023]);
        raw(&source, &[1; 1023]);
        assert_eq!(received.load(Ordering::SeqCst), 341);
        let old_raw = source.callbacks.lock().unwrap()[0].0.clone();
        owner.suspend_now().unwrap();
        old_raw(WarmRawBlock::I16(&[9999; 1023]));
        assert_eq!(received.load(Ordering::SeqCst), 341);
        assert!(lease.start_capture(Arc::new(|_| {})).await.is_err());
        lease.stop_capture().await.unwrap();
        owner.close().await.unwrap();
        assert_eq!(source.opens.load(Ordering::SeqCst), 1);
        owner.resume().unwrap();
        assert!(!owner.shared.control.lock().unwrap().desired);
        owner.prewarm().await.unwrap();
        old_raw(WarmRawBlock::I16(&[9999; 1023]));
        assert_eq!(received.load(Ordering::SeqCst), 341);
        assert!(owner.is_warm_ready());
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn stop_after_first_large_block_chunk_matches_cold_accepted_and_residual() {
        use rubato::Resampler;
        for rate in [16_000, 44_100, 48_000] {
            let (source, owner) = setup();
            source.sample_rate.store(rate, Ordering::SeqCst);
            let input_chunk = if rate == 48_000 { 1023 } else { 1024 };
            let input = vec![1200; input_chunk * 4 + 17];
            // Independent reference for the legacy SystemAudioCapture loop:
            // normalize -> gate -> enqueue, Stop happens at the first enqueue.
            let mut cold_buffer = input.clone();
            let mut cold_normalized = 0;
            let mut cold_accepted = Vec::new();
            let mut cold_running = true;
            let mut sinc = (rate == 44_100)
                .then(|| SystemAudioCapture::create_resampler(44_100, 16_000, 1).unwrap());
            while cold_buffer.len() >= input_chunk {
                let chunk: Vec<_> = cold_buffer.drain(..input_chunk).collect();
                let output = if let Some(sinc) = sinc.as_mut() {
                    let floats: Vec<_> = chunk
                        .iter()
                        .map(|sample| *sample as f32 / 32767.0)
                        .collect();
                    SystemAudioCapture::f32_to_i16(&sinc.process(&[floats], None).unwrap()[0])
                } else if rate == 48_000 {
                    SystemAudioCapture::downsample_integer_average(&chunk, 3)
                } else {
                    chunk
                };
                cold_normalized += output.len();
                if !cold_running {
                    break;
                }
                cold_accepted.extend(output);
                cold_running = false;
            }

            let actual = Arc::new(Mutex::new(Vec::new()));
            let delivered = actual.clone();
            let stopping = Arc::downgrade(&owner);
            let mut lease = owner.lease();
            lease.set_capture_identity(Some(AudioCaptureIdentity {
                run_id: 1,
                generation: 1,
            }));
            lease
                .start_capture(Arc::new(move |chunk| {
                    delivered.lock().unwrap().extend(chunk.data);
                    // Deterministic stop at the first downstream enqueue, while its
                    // delivery permit is still in flight. No sleeps or polling.
                    stopping.upgrade().unwrap().invalidate_now().unwrap();
                }))
                .await
                .unwrap();
            raw(&source, &vec![9999; input_chunk]); // Excluded boundary callback.
            raw(&source, &input);
            assert_eq!(
                *actual.lock().unwrap(),
                cold_accepted,
                "accepted rate={rate}"
            );
            let accounting = lease
                .lease
                .as_ref()
                .unwrap()
                .payload
                .processor
                .lock()
                .unwrap()
                .accounting();
            assert_eq!(
                accounting,
                (cold_normalized, cold_buffer.len()),
                "normalized/residual rate={rate}"
            );
            lease.stop_capture().await.unwrap();
            owner.close().await.unwrap();
        }
    }
}
