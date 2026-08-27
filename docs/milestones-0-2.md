# Milestones 0-2 backlog

## Milestone 0 — repository skeleton

- [x] Rust workspace with `core` and `app` crates.
- [x] Stable transcript, speaker, session-state, and event schemas.
- [x] Pure timestamp-overlap reconciliation with tests.
- [x] Sample campaign layout with a GM-private fixture.
- [x] Transitional audio/tool validation and a 10-second ALSA capture command.
- [x] Architecture decision record and runnable instructions.
- [ ] Add JSONL append-only event writer with crash-safe line writes.
- [ ] Add service health model and structured logs.
- [ ] Add a tiny local HTTP page that displays fixture transcript events.
- [ ] Add a read-only campaign loader configured for `/home/toby/src/family-dnd`;
  default all imported documents to `gm_private` until an allowlist exists.
- [x] Add an in-process microphone capture boundary with `cpal`.

## Milestone 1 — live transcription

- [x] Add `cpal` microphone capture owned by the Rust application.
- [x] Add Rust-native model download/cache/checksum handling.
- [x] Integrate the embedded Whisper backend; retain whisper.cpp as a
  replaceable comparison backend.
- [x] Add an embedded Whisper backend boundary using `whisper-rs`.
- [x] Wire native capture windows through resampling into embedded Whisper and
  the agent fan-out command.
- [ ] Install and benchmark whisper.cpp models on the intended machine as a
  backend comparison, not as a runtime prerequisite.
- [ ] Define a versioned STT adapter message: provisional/final segment,
  timestamps, text, confidence, source sequence.
- [x] Feed microphone chunks to the embedded Whisper backend.
- [ ] Reconcile revisions by stable source sequence, not text matching.
- [ ] Persist raw STT messages and expose a rolling transcript endpoint.
- [ ] Show latency, model, capture device, and failure state in the UI.
- [ ] Verify a clean-machine runtime needs only the packaged executable and
  first-run model downloads.
- [ ] Run a 30-minute stability test and record CPU/RAM/latency/accuracy.

## Milestone 2 — live diarization

- [ ] Record a labelled 3-5 person tabletop test set: turns, interruption,
  laughter, overlap, distance, and background noise.
- [ ] Benchmark sherpa-onnx speaker segmentation/embeddings and pyannote.audio
  as an offline quality baseline.
- [ ] Benchmark a streaming candidate against latency and diarization error.
- [ ] Define speaker-segment JSON messages and service restart semantics.
- [ ] Integrate overlap attribution and preserve unknown speaker state.
- [ ] Add manual speaker-to-person/character mappings.
- [ ] Display provisional attribution separately from finalized attribution.
- [ ] Replay recordings to measure regression without requiring a live table.
- [ ] Add campaign retrieval fixtures from `family-dnd` without modifying that
  repository; verify private scope is retained through context construction.
- [x] Define a shared transcript context and configurable agent fan-out.
- [x] Add deterministic recorder, live-summary, and GM next-step fixtures.
