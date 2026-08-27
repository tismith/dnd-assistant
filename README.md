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
```

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
