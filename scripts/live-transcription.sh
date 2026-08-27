#!/usr/bin/env bash
set -euo pipefail

: "${WHISPER_STREAM:?Set WHISPER_STREAM to the whisper.cpp whisper-stream binary}"
: "${WHISPER_MODEL:?Set WHISPER_MODEL to a ggml Whisper model}"

if [[ ! -x "$WHISPER_STREAM" ]]; then
  echo "WHISPER_STREAM is not executable: $WHISPER_STREAM" >&2
  exit 1
fi
if [[ ! -f "$WHISPER_MODEL" ]]; then
  echo "WHISPER_MODEL does not exist: $WHISPER_MODEL" >&2
  exit 1
fi

exec "$WHISPER_STREAM" -m "$WHISPER_MODEL" -t "${WHISPER_THREADS:-4}" \
  -c "${WHISPER_CAPTURE_DEVICE:-0}" \
  --step "${WHISPER_STEP_MS:-500}" --length "${WHISPER_LENGTH_MS:-5000}"

