# D&D Assistant

Local-first live D&D copilot. The repository starts with the smallest vertical
slice: local audio capture, stable transcript domain types, and a pure
timestamp-overlap reconciler.

## Current status

Milestone 0 is in progress. The Rust workspace builds and tests the domain
boundary. The current `record` command is a transitional diagnostic that
captures 16 kHz mono PCM WAV audio through ALSA. The target runtime is a single
Rust executable with in-process capture/inference and first-run model
downloads; whisper.cpp is not intended to remain a user-installed prerequisite.

```sh
cargo test
cargo run -p dnd-assistant -- validate
cargo run -p dnd-assistant -- reconcile-demo
cargo run -p dnd-assistant -- record data/spike.wav
cargo run -p dnd-assistant -- replay agents.example.json fixtures/transcript.jsonl data/replay
```

The replay command proves the agent fan-out without audio or network access.
Each enabled agent receives the current segment and a rolling 20-segment
window, plus the configured campaign Markdown contents. The built-in agents
write a JSONL recorder, a running Markdown summary, and GM next-step options.
Replace the built-in handlers with model-backed handlers later while retaining
the same context contract.

To use the family campaign context, change `campaign_context` in a private copy
of `agents.example.json` to:

```json
["/home/toby/src/family-dnd/campaign/CAMPAIGN_CONTEXT.md", "/home/toby/src/family-dnd/campaign/CANON.md", "/home/toby/src/family-dnd/plot/OPEN_THREADS.md"]
```

That repository is GM-facing; do not use these files for a player-facing agent
until a reviewed public allowlist exists.

For the next live transcription check, build whisper.cpp with its `whisper-stream`
example and run:

```sh
WHISPER_STREAM=/absolute/path/to/whisper-stream \
WHISPER_MODEL=/absolute/path/to/ggml-base.en.bin \
./scripts/live-transcription.sh
```

The script uses the upstream microphone streaming example, while the Rust
application remains responsible for lifecycle and future event ingestion.

## Campaign context

Use `config.example.toml` as the starting point for pointing at
`/home/toby/src/family-dnd`. That repository is GM-facing, so it is treated as
private until a reviewed public allowlist exists. The assistant only reads it;
it does not write campaign notes.

## Design documents

- [ADR-0001: Local-first pipeline](docs/adr/0001-local-first-pipeline.md)
- [Milestones 0-2 backlog](docs/milestones-0-2.md)
