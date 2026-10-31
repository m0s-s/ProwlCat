#!/usr/bin/env bash
# Regenerate the README animations (assets/gifs/). Run after bumping the version and building the
# release binary:
#   cargo build --release -p prowlcat-cli && scripts/gifs.sh
# Needs python3, rsvg-convert (librsvg) and ffmpeg. Shares the throwaway `pyte` virtualenv with
# scripts/screenshots.sh.
set -eu
cd "$(dirname "$0")/.."
venv=target/screenshots-venv
if [ ! -x "$venv/bin/python" ]; then
  python3 -m venv "$venv"
  "$venv/bin/pip" -q install pyte
fi
exec "$venv/bin/python" scripts/gifs.py "$@"
