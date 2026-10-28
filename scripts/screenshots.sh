#!/usr/bin/env bash
# Regenerate the README screenshots (assets/screenshots/). Run after bumping the version and
# building the release binary:
#   cargo build --release -p prowlcat-cli && scripts/screenshots.sh
# Needs python3, rsvg-convert (librsvg) and, optionally, ImageMagick. A throwaway virtualenv with
# `pyte` is kept under target/.
set -eu
cd "$(dirname "$0")/.."
venv=target/screenshots-venv
if [ ! -x "$venv/bin/python" ]; then
  python3 -m venv "$venv"
  "$venv/bin/pip" -q install pyte
fi
exec "$venv/bin/python" scripts/screenshots.py
