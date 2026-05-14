# AGENTS.md

- Backend repo: `/Users/belief/dev/projects/VoicetextAI/backend`.
- Change backend transcription code only when Deepgram docs, tests, or real audio output show a clear issue. Keep fixes narrow because STT regressions are high risk.
- Production backend is deployed on Render. Use Render MCP for production deploy status, logs, and backend health checks.
- For this frontend repo, use `pnpm tauri dev` or `pnpm tauri:dev` for local desktop development. This repo does not currently define the HackInterview `copy-helper:brave:restart-cdp` script.
- Deepgram streaming UI rules:
  - append every non-empty `is_final=true` transcript segment to the current utterance buffer
  - move buffered text into final UI text only on `speech_final=true`
  - also flush buffered text when backend marks an explicit Deepgram `Finalize` result with `from_finalize=true`
  - do not treat `is_final=true` alone as a complete utterance
  - websocket control messages must be JSON text frames, including `KeepAlive`, `Finalize`, and `CloseStream`
- Real audio files for manual/e2e transcription checks:
  - `/Users/belief/Documents/2026-05-14 23.56.41.ogg`
  - `/Users/belief/Documents/2026-05-14 23.57.41.ogg`
  - `/Users/belief/Documents/2026-05-14 23.57.56.ogg`
