# AGENTS.md

## Purpose

This repository is the local-first D&D Assistant. It is intentionally being
built as small runnable vertical slices. The primary early risks are real-room
audio, diarization quality, latency, and useful GM suggestions; do not expand
the architecture to solve later features before those risks are measured.

## Working rules

- Keep raw audio local by default.
- Treat transcript and event records as append-only source data; derived state
  should be rebuildable where practical.
- Keep STT, diarization, and LLM providers behind replaceable process or trait
  boundaries. Do not add ML FFI to Rust merely for language consistency.
- Provisional transcript and speaker attribution are valid live states.
- Never let an agent failure stop capture or transcript persistence.
- Keep commits narrow and coherent. Build, test, and document each increment.
- Do not claim microphone, diarization, browser, runtime, or packaged behavior
  until it has been exercised on this machine or in a clearly named fixture.

## Campaign context safety

The reference campaign repository is `/home/toby/src/family-dnd`. It is a
GM-facing repository and may contain spoilers, secrets, theories, and future
prep. Treat the entire repository as `gm_private` by default. Do not infer
player visibility from directory names or Markdown headings. A player-facing
context requires an explicit reviewed allowlist or machine-readable scope
metadata.

When loading it, start with:

1. `campaign/CAMPAIGN_CONTEXT.md`
2. `campaign/CANON.md`
3. the latest played session (not merely the latest prep file)
4. `plot/OPEN_THREADS.md`

Never mutate that repository from this project’s context loader. Keep imported
context read-only and make its source path visible in diagnostics.

## Validation

Useful checks from the repository root:

```sh
cargo fmt --all -- --check
cargo test --offline
bash -n scripts/live-transcription.sh
git diff --check
```

External tools are optional prerequisites for the first spike. Use
`cargo run -p dnd-assistant -- validate` to report whether ALSA `arecord` and
the upstream whisper.cpp `whisper-stream` binary are available.

