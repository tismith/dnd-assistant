# D&D Assistant

Local-first live D&D copilot. The repository starts with the smallest vertical
slice: local audio capture, stable transcript domain types, and a pure
timestamp-overlap reconciler.

## Current status

Milestone 0 is in progress. The Rust workspace builds and tests the domain
boundary. The `record` command captures a ten-second PCM WAV through native
CPAL. The target runtime is a single
Rust executable with in-process capture/inference and first-run model
downloads; whisper.cpp is not intended to remain a user-installed prerequisite.

```sh
cargo test
cargo run -p dnd-assistant -- validate
cargo run -p dnd-assistant -- audio-info
cargo run -p dnd-assistant -- capture
# From a campaign workspace: uses discovered config, or built-in defaults.
cd /path/to/campaign
cargo run --manifest-path /home/toby/src/dnd-assistant/Cargo.toml -p dnd-assistant -- live
cargo run -p dnd-assistant -- reconcile-demo
cargo run -p dnd-assistant -- record
cargo run -p dnd-assistant -- transcribe-wav /path/to/model.bin /path/to/recording.wav agents.example.json
cargo run -p dnd-assistant -- session-end agents.example.json /path/to/session
cargo run -p dnd-assistant -- replay agents.example.json fixtures/transcript.jsonl "${XDG_DATA_HOME:-$HOME/.local/share}/dnd-assistant/sessions/replay"
cat fixtures/transcript.jsonl | cargo run -p dnd-assistant -- stream agents.example.json
```

`replay` proves the agent fan-out deterministically. `stream` consumes the
same JSONL contract one line at a time, which is the live boundary for the
future embedded transcription engine.

`audio-info`, `capture`, `record`, and `live` use the in-process `cpal` capture
crate. `capture` reports normalized raw chunks; `record` writes a standard PCM
WAV. `transcribe-wav` reads that WAV format directly, resamples it, and sends
the resulting finalized transcript through the same event log and agents as
the live path. The `arecord` check in `validate` remains
only as an optional diagnostic for the older development spike.

`live` is the in-process transcription path. It captures five-second windows,
downmixes/resamples them to 16 kHz mono, runs the embedded Whisper backend,
and sends finalized segments through the configured agents on a transcription
worker, keeping microphone draining responsive with bounded backpressure. Its first argument
may be an existing model path or an HTTP(S) model URL; URL models are cached in
the XDG cache directory (`$XDG_CACHE_HOME/dnd-assistant/models`, or
`~/.cache/dnd-assistant/models`) by the Rust-native model manager. Session
outputs default to a per-session directory beneath
`$XDG_DATA_HOME/dnd-assistant/sessions`, or
`~/.local/share/dnd-assistant/sessions`.
If the current campaign workspace contains
`campaign/TRANSCRIPTION_GLOSSARY.md`, its text is supplied to Whisper as an
initial vocabulary prompt for every audio window. Use it for proper nouns,
locations, factions, creatures, and other setting-specific spellings. The
file is optional, read-only, and bounded before being passed to the model.
Each enabled agent receives the current segment and a rolling 20-segment
window, plus the configured campaign Markdown contents. Custom model agents
can set an inline `instruction`, a longer `prompt_file`, and explicit
`workspace_paths` containing files or directories of Markdown documents. Each
agent receives only its own selected workspace documents, which are read-only
and include their source paths in the serialized context. The built-in agents
write a JSONL recorder, a running Markdown summary, and GM next-step options.
Each agent can set an optional `instruction` and `run_every_segments` cadence;
the latter is useful for agents that should inspect the rolling context every
few transcript segments rather than on every update.
Set `include_campaign_context` to `false` for an agent that should not receive
the legacy global campaign context; it can then use only its explicit
`workspace_paths`. Set `workspace_query` to a fixed lexical query when the
agent needs a stable slice of the workspace; otherwise the recent transcript
is used to select relevant documents. The workspace index also exposes bounded
local `list`, `read`, and `search` operations for the future tool-calling path.
When `workspace_paths` is empty, the process current directory is used as the
workspace root. This is the normal campaign workflow: change into the campaign
repository, then start the assistant. Likewise, an empty `write_paths` for the
session editor means that same current directory, while still requiring
`--apply` before any campaign files change.
When `live` is started without a model, it downloads the default
`ggml-base.en.bin` model on first use and caches it in the XDG cache directory.
When it is started without a config, it checks
`$XDG_CONFIG_HOME/dnd-assistant/agents.json` (or `~/.config/dnd-assistant/agents.json`)
and then `agents.json` in the current directory. If neither exists, the built-in
local agents are used. The application prints the UI URL, normally
`http://127.0.0.1:8787/`, when the live server starts.
Replace the built-in handlers with model-backed handlers later while retaining
the same context contract. Agent jobs are queued independently of capture and
model calls have a bounded timeout. It also starts a localhost UI at
`http://127.0.0.1:8787/`; set `DND_ASSISTANT_UI_ADDRESS` to change the bind
address. The UI shows the rolling transcript and latest output from each
enabled agent and provides Start, Pause/Resume, and Stop session controls.
The live command waits in `ready` until Start is pressed; set
`DND_ASSISTANT_AUTO_START=1` for headless or command-line use. If the UI
cannot bind, capture automatically starts and agent processing continues.
Every accepted segment is also appended to `events.jsonl` in the session
directory, independently of the configured agents, for replay and recovery.
Obviously silent windows are skipped before Whisper inference; adjust the
normalized RMS threshold with `DND_ASSISTANT_SILENCE_RMS` if the microphone
needs a different noise floor (the default is `0.005`).

An optional `llm` provider can be configured in the same JSON file for agents
with `"kind": "llm"`. Set the endpoint to `codex://local` to use the locally
installed Codex app-server and your existing Codex login. The assistant starts
an ephemeral read-only Codex thread for each agent run and passes only the
bounded context assembled by its workspace index. No campaign files are made
available to Codex outside that context. An OpenAI-compatible
`/chat/completions` endpoint is also supported; set `api_key_env` when it needs
a Bearer token. Network/model agents are disabled unless explicitly enabled,
and their failures do not stop capture or the other agents.

For a custom model agent, add an entry like:

```json
{
  "id": "continuity-checker",
  "kind": "llm",
  "enabled": true,
  "output": "continuity-checker.md",
  "prompt_file": "prompts/continuity-checker.md",
  "workspace_paths": [
    "/home/toby/src/family-dnd/campaign/CANON.md",
    "/home/toby/src/family-dnd/plot/OPEN_THREADS.md"
  ],
  "run_every_segments": 6
}
```

The repository includes focused prompts for `gm-copilot`, `session-guide`,
`continuity-watcher`, `player-interest`, `session-scribe`, and
`session-supervisor`. The first five are live read-only observers; the
supervisor runs only through `session-end`. `session-guide` specifically
cross-checks the upcoming session prep against the live scene and campaign
context, while treating prep as intention rather than established fact.
They are disabled in `agents.example.json` by default because model agents
require an explicitly configured LLM provider. Enable only the agents you want
and keep their `workspace_paths` scoped to the campaign material they need.

Live agents write suggestions and proposed notes to their configured session
output only. They do not mutate campaign files. The optional session editor
below is the explicit, reviewable campaign-update path.

The optional `session_editor` agent is run explicitly after a session:

```sh
cargo run -p dnd-assistant -- session-end agents.json /path/to/session
cargo run -p dnd-assistant -- session-end agents.json /path/to/session --apply
```

It reads the session transcript, all session agent outputs, and its configured
workspace paths. It writes a high-level summary and structured update plan into
the session directory. `--apply` validates every exact text replacement against
the configured `write_paths`, creates backups under the session directory, and
then applies the coordinated campaign edits.

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

Run the assistant from the campaign repository to make it the canonical
workspace; no campaign path configuration is required. For custom agents,
copy `agents.example.json` to the XDG config location or to `agents.json` in
the campaign directory and edit the agent definitions. The reference campaign
repository is GM-facing, so it is treated as private until a reviewed public
allowlist exists. The live assistant only reads it; it does not write campaign
notes.

## Design documents

- [ADR-0001: Local-first pipeline](docs/adr/0001-local-first-pipeline.md)
- [Milestones 0-2 backlog](docs/milestones-0-2.md)
