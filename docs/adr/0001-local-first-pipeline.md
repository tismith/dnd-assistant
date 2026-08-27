# ADR-0001: Local-first, replaceable live-session pipeline

## Status

Accepted for the first implementation increment.

## Decision

Use a Rust application and stable internal domain events. The distributable
application should own microphone capture and inference in-process so an end
user does not need to install helper programs. Keep backend interfaces
replaceable and retain process boundaries as an option for development and
benchmarking.
Keep raw append-only JSONL records as the source record; use SQLite later for
indexes and projections. Serve a small local web UI over HTTP plus WebSocket
updates once the live transcript exists.

Initial concrete choices:

- Audio: `cpal` behind an `AudioCapture` boundary. The existing ALSA
  `arecord` command is retained only as a diagnostic spike until in-process
  capture lands.
- In-process inference: `sherpa-onnx` Rust bindings, statically linked where
  supported, with streaming ASR/VAD and speaker APIs. Models are downloaded
  and cached by the application at first launch.
- STT benchmark: upstream whisper.cpp through `whisper-rs` or the existing
  `whisper-stream` harness. It remains a replaceable backend, not a runtime
  prerequisite.
- Diarization: benchmark sherpa-onnx speaker segmentation/embedding support
  against pyannote.audio as an offline quality baseline. Do not claim that
  offline diarization is streaming until the real tabletop test proves it.
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

The distributable app has fewer runtime prerequisites and can manage model
downloads, cache locations, versions, and checksums itself. The tradeoff is a
larger native binary and more platform-specific packaging/testing. Build-time
toolchains may still be needed by developers; “no other programs” is a runtime
distribution goal, not a promise that compiling native ML crates requires no
toolchain.

The diarization choice remains intentionally unresolved. Current pyannote
pipelines provide strong offline diarization but do not by themselves prove
low-latency four-person tabletop performance. This must be measured with real
room recordings before architecture hardens.

## Runtime prerequisites

The application cannot eliminate the operating system's audio stack or device
drivers. It can eliminate user-installed command-line helpers and language
runtimes. Optional model downloads must be explicit, resumable, checksum
verified, stored in an application cache, and usable offline after download.
Use XDG Base Directory locations on Linux: models in
`$XDG_CACHE_HOME/dnd-assistant/models` and durable recordings/session data in
`$XDG_DATA_HOME/dnd-assistant/sessions`, with standard home-directory fallbacks.

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
