# ADR-0001: Local-first, replaceable live-session pipeline

## Status

Accepted for the first implementation increment.

## Decision

Use a Rust orchestrator and stable internal domain events. Capture audio and
run speech recognition/diarization in independently restartable processes.
Keep raw append-only JSONL records as the source record; use SQLite later for
indexes and projections. Serve a small local web UI over HTTP plus WebSocket
updates once the live transcript exists.

Initial concrete choices:

- Audio: ALSA through `arecord` for the first Linux spike; `cpal` behind an
  `AudioCapture` boundary when capture becomes application-owned.
- STT: upstream whisper.cpp `whisper-stream` for the spike, then its server or
  a narrow adapter process. No Rust FFI in the MVP.
- Diarization: Python service behind timestamped JSONL/stdin or localhost HTTP;
  benchmark pyannote.audio offline and a genuinely incremental option before
  committing to a live implementation.
- IPC: newline-delimited JSON over localhost process pipes first; HTTP for
  long-lived services that need independent restart/health checks.
- Web: axum and Tokio once the core loop is proven.
- Frontend: small TypeScript-free HTML/JavaScript first; introduce a bundler
  only when interaction requires it.
- Persistence: JSONL transcript/events plus SQLite projections; Markdown
  campaign documents with explicit `public` and `gm_private` scopes. An
  existing GM-facing repository without an enforced allowlist defaults to
  `gm_private`; this applies to `/home/toby/src/family-dnd`.

## Consequences

The first spike has fewer moving parts and exposes the actual audio/model
constraints early. The tradeoff is that the upstream stream example is not yet
an application-integrated transcript event source; that adapter is Milestone 1.

The diarization choice remains intentionally unresolved. Current pyannote
pipelines provide strong offline diarization but do not by themselves prove
low-latency four-person tabletop performance. This must be measured with real
room recordings before architecture hardens.

## Campaign context integration

The first external campaign fixture is `/home/toby/src/family-dnd`. Its
maintained orientation file is `campaign/CAMPAIGN_CONTEXT.md`, followed by
`campaign/CANON.md`, the latest played session, and `plot/OPEN_THREADS.md`.
The repository's `AGENTS.md` describes it as GM-facing and allows spoilers, so
the context loader must not infer player visibility from ordinary Markdown
location. A reviewed allowlist or explicit metadata is required before any
player-facing agent can consume those documents.

## Rejected for now

Docker Compose, a vector database, full combat state, automatic voice identity,
desktop packaging, and six agent abstractions are outside the first proof.
