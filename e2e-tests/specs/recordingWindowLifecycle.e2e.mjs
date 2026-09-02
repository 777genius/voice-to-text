import assert from 'node:assert/strict';
import {
  emitEvent,
  ensureE2E,
  findWindowHandleByLabel,
  invoke,
  waitFor,
} from '../helpers/tauriE2e.mjs';

const WILL_HIDE = 'recording:window-will-hide-for-hotkey-stop';
// Exceeds the native 220 ms hide delay and Vue's 260 ms close reset.
const OBSERVATION_MS = 800;
const TRANSCRIPT = 'window lifecycle regression fixture';

async function visibility() {
  const result = await browser.executeAsync((done) => {
    window.__E2E__.getWindowVisibility()
      .then((visible) => done({ visible }))
      .catch((error) => done({ error: String(error) }));
  });
  assert.equal(result.error, undefined);
  return result.visible;
}

async function showAndSettle() {
  const result = await browser.executeAsync(async (done) => {
    let sawOpening = false;
    const observer = new MutationObserver((mutations) => {
      if (document.querySelector('.popover.mini.mini-opening') || mutations.some((mutation) =>
        (mutation.oldValue || '').split(/\s+/).includes('mini-opening'))) {
        sawOpening = true;
      }
    });
    observer.observe(document.documentElement, {
      subtree: true, attributes: true, attributeFilter: ['class'], attributeOldValue: true,
    });
    try {
      await window.__E2E__.invoke('show_recording_window');
      const deadline = Date.now() + 10_000;
      while (Date.now() < deadline) {
        const mini = document.querySelector('.popover.mini');
        if (sawOpening && mini && !mini.matches('.mini-opening, .mini-closing, .mini-animation-reset')) {
          const epoch = await window.__E2E__.invoke('get_recording_window_epoch');
          done({ epoch, visible: await window.__E2E__.getWindowVisibility() });
          return;
        }
        await new Promise((resolve) => setTimeout(resolve, 25));
      }
      throw new Error('new native show never completed its Vue opening animation');
    } catch (error) {
      done({ error: String(error) });
    } finally {
      observer.disconnect();
    }
  });
  assert.equal(result.error, undefined);
  assert.equal(result.visible, true);
  const epoch = result.epoch;
  assert.ok(Number.isSafeInteger(epoch) && epoch > 0, `invalid window epoch: ${epoch}`);
  return epoch;
}

async function seedTranscript() {
  await browser.execute((text) => window.__E2E__.seedRecordingTranscript(text), TRANSCRIPT);
  await waitFor(async () => await browser.execute((text) =>
    document.querySelector('.mini-transcription-text-inner')?.textContent?.trim() === text,
  TRANSCRIPT), { intervalMs: 25 });
}

// Observe the entire interval rather than passing on the first visible sample.
// The observer also remembers short closing transitions between IPC samples.
async function observeEvent(windowEpoch, { stale, durationMs = OBSERVATION_MS }) {
  const result = await browser.executeAsync(async (event, epoch, text, duration, done) => {
    let sawClosing = Boolean(document.querySelector('.mini-closing'));
    let observer;
    try {
      observer = new MutationObserver((mutations) => {
        if (document.querySelector('.mini-closing') || mutations.some((mutation) =>
          (mutation.oldValue || '').split(/\s+/).includes('mini-closing'))) {
          sawClosing = true;
        }
      });
      observer.observe(document.documentElement, {
        subtree: true, attributes: true, attributeFilter: ['class'], attributeOldValue: true,
      });
      await window.__E2E__.emitEvent(event, { windowEpoch: epoch });
      const started = Date.now();
      let samples = 0;
      let lostVisibility = false;
      let lostTranscript = false;
      do {
        lostVisibility ||= !(await window.__E2E__.getWindowVisibility());
        lostTranscript ||= document.querySelector('.mini-transcription-text-inner')
          ?.textContent?.trim() !== text;
        samples += 1;
        await new Promise((resolve) => setTimeout(resolve, 25));
      } while (Date.now() - started < duration || samples < 2);
      done({ sawClosing, lostVisibility, lostTranscript, samples, elapsed: Date.now() - started });
    } catch (error) {
      done({ error: String(error) });
    } finally {
      observer?.disconnect();
    }
  }, WILL_HIDE, windowEpoch, TRANSCRIPT, durationMs);

  assert.equal(result.error, undefined);
  assert.ok(result.elapsed >= durationMs, 'must observe the entire requested interval');
  assert.ok(result.samples >= 2, 'must sample native visibility more than once');
  assert.equal(result.lostVisibility, false, 'the event alone must not hide the native window');
  assert.equal(result.sawClosing, !stale, stale
    ? 'stale event animated the reopened mini window closed'
    : 'positive control: current event did not reach the real Vue close handler');
  assert.equal(result.lostTranscript, !stale, stale
    ? 'stale event suppressed the reopened transcript'
    : 'positive control: current event did not suppress the transcript');
}

describe('recording window lifecycle (native window + Vue, no microphone)', () => {
  it('rejects stale hide IPC/events after reopen, including a bounded hide/show burst', async () => {
    await ensureE2E();
    await browser.switchToWindow(await findWindowHandleByLabel('main'));
    assert.equal(await invoke('get_recording_status'), 'Idle', 'fixture must not stop a live session');
    const original = await browser.execute(() => window.__E2E__.getAppConfig());

    try {
      // Both native sizing and Vue must use mini mode. A local Pinia override
      // alone would be reset by window-shown's native configuration refresh.
      await invoke('update_app_config', {
        showMiniRecordingWindow: true,
        playCompletionSound: false,
      });
      await browser.execute(() => window.__E2E__.useMiniRecordingLayout());
      let previousEpoch = await showAndSettle();
      await seedTranscript();
      // Positive control prevents a disconnected listener from passing stale checks.
      await observeEvent(previousEpoch, { stale: false });
      assert.equal(await invoke('hide_recording_window_if_current', {
        windowEpoch: previousEpoch,
      }), true, 'current epoch must actually hide the native window');
      assert.equal(await visibility(), false);

      let currentEpoch = await showAndSettle();
      assert.ok(currentEpoch > previousEpoch, 'reopen must advance the native epoch');
      await seedTranscript();
      assert.equal(await invoke('hide_recording_window_if_current', {
        windowEpoch: previousEpoch,
      }), false, 'old hide IPC must be rejected after reopen');
      await observeEvent(previousEpoch, { stale: true });

      // Exercise actual native hide/show repeatedly without invoking recording
      // start/stop, physical hotkeys, audio devices, or a speech provider.
      for (let iteration = 0; iteration < 5; iteration += 1) {
        previousEpoch = currentEpoch;
        await emitEvent(WILL_HIDE, { windowEpoch: previousEpoch });
        assert.equal(await invoke('hide_recording_window_if_current', {
          windowEpoch: previousEpoch,
        }), true);
        assert.equal(await visibility(), false);
        currentEpoch = await showAndSettle();
        assert.ok(currentEpoch > previousEpoch);
        await seedTranscript();
        assert.equal(await invoke('hide_recording_window_if_current', {
          windowEpoch: previousEpoch,
        }), false);
        // Represents an old WebView event delivered only after the new show.
        await observeEvent(previousEpoch, { stale: true, durationMs: 75 });
      }

      // Keep the final show visible through another full late-callback interval.
      await observeEvent(previousEpoch, { stale: true });
      assert.equal(await visibility(), true);
      assert.equal(await invoke('get_recording_status'), 'Idle');
    } finally {
      // Restore the persisted config and the current WebView layout without
      // re-showing an already visible native window. Re-applying native size and
      // position here can trip a GTK/X11 error trap after the hide/show stress;
      // the test has already verified the final window is visible above.
      await invoke('update_app_config', {
        showMiniRecordingWindow: original.showMiniRecordingWindow,
        playCompletionSound: original.playCompletionSound,
      });
      await browser.execute(() => {
        window.__E2E__.seedRecordingTranscript('');
        window.__E2E__.useFullRecordingLayout();
      });
    }
  });
});
