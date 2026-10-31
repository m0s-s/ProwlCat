#!/usr/bin/env python3
"""Record the README animations of the interactive interface (assets/gifs/*.gif).

Same idea as scripts/screenshots.py (the real release binary, in a pseudo-terminal, on a made-up
corpus of reserved `example.*` domains), but instead of one final screen it keeps every distinct
screen with the time it stayed up, and assembles them into a looping GIF with ffmpeg.

Usage:  scripts/gifs.sh
Needs: target/release/prowlcat, rsvg-convert, ffmpeg, and the `pyte` venv set up by gifs.sh.
Writes assets/gifs/*.gif and assets/gifs/VERSION (compared by scripts/release-check.sh).
"""
import collections
import fcntl
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time

import pyte

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import screenshots as S  # noqa: E402

OUT = os.path.join(S.ROOT, "assets", "gifs")
UP, DOWN, TAB, ESC = b"\x1b[A", b"\x1b[B", b"\t", b"\x1b"
ENTER = b"\r"

Cell = collections.namedtuple("Cell", "data fg bg bold reverse")


class Snap:
    """A frozen screen, shaped like pyte.Screen for S.to_svg."""

    def __init__(self, screen):
        self.buffer = [[Cell(c.data, c.fg, c.bg, c.bold, c.reverse) for c in (screen.buffer[y][x] for x in range(S.COLS))]
                       for y in range(S.ROWS)]

    def key(self):
        return tuple(tuple(r) for r in self.buffer)


def typed(text, gap=0.08):
    return [(c.encode(), gap) for c in text]


# name -> list of (keys, seconds to watch afterwards); the first screen is held for `lead` seconds.
SCENES = {
    # the whole idea in one take: type a target, watch it scan, walk the results, ask for help
    "tour": dict(lead=2.0, tail=2.5, steps=(
        [(b"", 0.6)] + typed("example.com") + [(b"", 0.8), (ENTER, 4.0), (b"", 1.2),
         (DOWN, 1.0), (DOWN, 1.0), (TAB, 1.0), (DOWN, 0.7), (DOWN, 0.7), (DOWN, 0.7), (b"?", 2.5)])),
    "commands": dict(lead=1.2, tail=2.0, steps=(
        [(b"/", 1.0), (DOWN, 0.8), (DOWN, 0.8), (DOWN, 0.8), (DOWN, 0.8), (UP, 0.8)]
        + [(b"s", 0.8), (b"t", 1.2)])),
    "results": dict(lead=0.5, tail=2.0, steps=(
        typed("example.com") + [(ENTER, 4.0), (DOWN, 0.9), (DOWN, 0.9), (UP, 0.9), (TAB, 0.9),
         (DOWN, 0.6), (DOWN, 0.6), (DOWN, 0.6), (DOWN, 0.6), (DOWN, 0.6)])),
}


def record(cwd, state, steps, lead, tail):
    env = dict(os.environ, TERM="xterm-256color", LANG="C.UTF-8", COLORTERM="truecolor",
               PROWLCAT_COLOR="truecolor", PROWLCAT_NO_ANIM="1", PROWLCAT_STATE_DIR=state,
               PROWLCAT_NO_HISTORY="1")
    for k in ("NO_COLOR", "PROWLCAT_UI", "PROWLCAT_LOG_FILE"):
        env.pop(k, None)
    screen = pyte.Screen(S.COLS, S.ROWS)
    stream = pyte.ByteStream(screen)
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execve(S.BIN, [S.BIN, "tui", "-p", "demo", "-r", "--no-splash"], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", S.ROWS, S.COLS, 0, 0))
    frames = []  # [snap, start time]

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([fd], [], [], 0.03)
            if ready:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return
                if not data:
                    return
                stream.feed(data)
            snap = Snap(screen)
            if not frames or frames[-1][0].key() != snap.key():
                frames.append([snap, time.time()])

    pump(1.5)
    frames[:] = frames[-1:]  # forget the startup, keep the screen it settled on
    frames[0][1] = time.time()
    pump(lead)
    for keys, wait in steps:
        if keys:
            os.write(fd, keys)
        pump(wait)
    pump(tail)
    end = time.time()
    try:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    except OSError:
        pass
    out = []
    for i, (snap, t) in enumerate(frames):
        nxt = frames[i + 1][1] if i + 1 < len(frames) else end
        out.append((snap, max(nxt - t, 0.04)))
    return out


def build(name, frames, tmp, scale):
    d = os.path.join(tmp, name)
    os.makedirs(d)
    lines = []
    for i, (snap, dur) in enumerate(frames):
        svg, png = os.path.join(d, f"{i:04d}.svg"), os.path.join(d, f"{i:04d}.png")
        S.to_svg(snap, svg)
        subprocess.run(["rsvg-convert", "-z", str(scale), svg, "-o", png], check=True)
        lines.append(f"file '{png}'\nduration {dur:.3f}")
    lines.append(f"file '{png}'")  # concat quirk: the last frame is repeated
    lst = os.path.join(d, "list.txt")
    with open(lst, "w") as f:
        f.write("\n".join(lines) + "\n")
    pal = os.path.join(d, "pal.png")
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "concat", "-safe", "0", "-i", lst,
                    "-vf", "palettegen=max_colors=96:stats_mode=full", pal], check=True)
    dest = os.path.join(OUT, f"{name}.gif")
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "concat", "-safe", "0", "-i", lst, "-i", pal,
                    "-lavfi", "paletteuse=dither=none:diff_mode=rectangle", "-loop", "0", dest], check=True)
    return dest, len(frames)


def main():
    for tool in ("rsvg-convert", "ffmpeg"):
        if not shutil.which(tool):
            sys.exit(f"{tool} not found")
    if not os.path.exists(S.BIN):
        sys.exit(f"{S.BIN} not found: run `cargo build --release -p prowlcat-cli` first")
    version = subprocess.run([S.BIN, "--version"], capture_output=True, text=True, check=True).stdout.split()[-1]
    scale = float(os.environ.get("GIF_SCALE", "1.0"))
    only = sys.argv[1:]
    os.makedirs(OUT, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        S.make_corpus(tmp)
        for name, sc in SCENES.items():
            if only and name not in only:
                continue
            frames = record(tmp, os.path.join(tmp, "state"), sc["steps"], sc["lead"], sc["tail"])
            dest, n = build(name, frames, tmp, scale)
            print(f"wrote {os.path.relpath(dest, S.ROOT)}: {n} frames, {os.path.getsize(dest) // 1024} KiB")
    with open(os.path.join(OUT, "VERSION"), "w") as f:
        f.write(version + "\n")


if __name__ == "__main__":
    main()
