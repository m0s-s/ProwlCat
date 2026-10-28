#!/usr/bin/env python3
"""Regenerate the README screenshots of the interactive interface.

Runs the release binary in a pseudo-terminal on a small made-up corpus (reserved `example.*`
domains and documentation IP addresses, never real data), feeds it keystrokes, renders the final
screen of each scene as SVG and converts it to a PNG.

Usage:  scripts/screenshots.sh          (sets up a venv with `pyte`, then runs this)

Needs: target/release/prowlcat, `rsvg-convert` (librsvg) and, optionally, ImageMagick to shrink
the PNGs. Writes assets/screenshots/tui-*.png and assets/screenshots/VERSION, which
scripts/release-check.sh compares with the version being released.
"""
import fcntl
import gzip
import os
import pty
import random
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time

import pyte

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.environ.get("PROWLCAT_BIN", os.path.join(ROOT, "target", "release", "prowlcat"))
OUT = os.path.join(ROOT, "assets", "screenshots")

COLS, ROWS = 112, 34
CW, CH, FS = 9.0, 18.0, 15.0
DEF_FG, DEF_BG = "d4d4d4", "0b0d12"
NAMED = {
    "black": "000000", "red": "cd3131", "green": "0dbc79", "brown": "e5e510", "blue": "2472c8",
    "magenta": "bc3fbc", "cyan": "11a8cd", "white": "e5e5e5",
    "brightblack": "666666", "brightred": "f14c4c", "brightgreen": "23d18b", "brightbrown": "f5f543",
    "brightblue": "3b8eea", "brightmagenta": "d670d6", "brightcyan": "29b8db", "brightwhite": "ffffff",
}

# name -> (extra args after `tui -p demo -r --no-splash`, [(keys, seconds to wait)])
SCENES = {
    "home": ([], []),
    "commands": ([], [(b"/", 0.6)]),
    "results": ([], [(b"example.com", 0.3), (b"\r", 4.0)]),
    "help": ([], [(b"example.com", 0.3), (b"\r", 4.0), (b"?", 0.6)]),
}


def make_corpus(root):
    """A deterministic corpus of invented accounts."""
    random.seed(7)
    first = ["alice", "bob", "carol", "dave", "erin", "frank", "grace", "heidi", "ivan", "judy", "mallory",
             "niaj", "olivia", "peggy", "rupert", "sybil", "trent", "victor", "wendy"]
    domains = ["example.com", "mail.example.com", "example.org", "acme.example", "corp.example.net"]
    passwords = ["Winter2024", "hunter2", "correcthorse", "Password1!", "qwerty123", "letmein", "Summer#19", "dragon99"]

    def lines(n, doms, urls=False):
        out = []
        for _ in range(n):
            user = random.choice(first) + str(random.randrange(10, 999))
            dom = random.choice(doms)
            if urls and random.random() < 0.3:
                out.append(f"https://{dom}/login:{user}:{random.choice(passwords)}")
            else:
                out.append(f"{user}@{dom}:{random.choice(passwords)}")
            if random.random() < 0.08:
                out.append(f"{random.choice(['203.0.113', '198.51.100', '10.20'])}.{random.randrange(1, 250)} admin {random.choice(passwords)}")
        return "\n".join(out) + "\n"

    demo = os.path.join(root, "demo")
    os.makedirs(demo)
    with open(os.path.join(demo, "acme-2019.txt"), "w") as f:
        f.write(lines(9000, ["acme.example", "example.com", "example.org"]))
    with gzip.open(os.path.join(demo, "forum-dump.txt.gz"), "wt") as f:
        f.write(lines(12000, domains, True))
    with open(os.path.join(demo, "combo-small.txt"), "w") as f:
        f.write(lines(3000, ["corp.example.net", "example.com"]))


def color(c, default):
    if c == "default":
        return default
    if c in NAMED:
        return NAMED[c]
    return c if len(c) == 6 else default


def shoot(cwd, state, extra, steps):
    """Run the interface in a pty, send the keys, return the final screen."""
    env = dict(os.environ, TERM="xterm-256color", LANG="C.UTF-8", COLORTERM="truecolor",
               PROWLCAT_COLOR="truecolor", PROWLCAT_NO_ANIM="1", PROWLCAT_STATE_DIR=state,
               PROWLCAT_NO_HISTORY="1")
    for k in ("NO_COLOR", "PROWLCAT_UI", "PROWLCAT_LOG_FILE"):
        env.pop(k, None)
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.ByteStream(screen)
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execve(BIN, [BIN, "tui", "-p", "demo", "-r", "--no-splash"] + extra, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([fd], [], [], 0.05)
            if ready:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return
                if not data:
                    return
                stream.feed(data)

    pump(1.5)
    for keys, wait in steps:
        os.write(fd, keys)
        pump(wait)
    try:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    except OSError:
        pass
    return screen


def to_svg(screen, path):
    w, h = COLS * CW, ROWS * CH
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">',
           f'<rect width="{w}" height="{h}" fill="#{DEF_BG}"/>',
           f'<g font-family="DejaVu Sans Mono, Liberation Mono, monospace" font-size="{FS}">']
    for y in range(ROWS):
        row = screen.buffer[y]
        for x in range(COLS):
            c = row[x]
            fg, bg = color(c.fg, DEF_FG), color(c.bg, DEF_BG)
            if c.reverse:
                fg, bg = bg, fg
            px, py = x * CW, y * CH
            ch = c.data
            if ch == "▀":
                out.append(f'<rect x="{px}" y="{py}" width="{CW + .4}" height="{CH / 2 + .2}" fill="#{fg}"/>')
                out.append(f'<rect x="{px}" y="{py + CH / 2}" width="{CW + .4}" height="{CH / 2 + .2}" fill="#{bg}"/>')
                continue
            if ch == "▄":
                out.append(f'<rect x="{px}" y="{py}" width="{CW + .4}" height="{CH / 2 + .2}" fill="#{bg}"/>')
                out.append(f'<rect x="{px}" y="{py + CH / 2}" width="{CW + .4}" height="{CH / 2 + .2}" fill="#{fg}"/>')
                continue
            if ch == "█":
                out.append(f'<rect x="{px}" y="{py}" width="{CW + .4}" height="{CH + .4}" fill="#{fg}"/>')
                continue
            if bg != DEF_BG:
                out.append(f'<rect x="{px}" y="{py}" width="{CW + .4}" height="{CH + .2}" fill="#{bg}"/>')
            if ch.strip():
                weight = ' font-weight="bold"' if c.bold else ""
                esc = ch.replace("&", "&amp;").replace("<", "&lt;")
                out.append(f'<text x="{px}" y="{py + CH - 4.5}" fill="#{fg}"{weight}>{esc}</text>')
    out.append("</g></svg>")
    with open(path, "w") as f:
        f.write("\n".join(out))


def main():
    if not os.path.exists(BIN):
        sys.exit(f"{BIN} not found: run `cargo build --release -p prowlcat-cli` first")
    if not shutil.which("rsvg-convert"):
        sys.exit("rsvg-convert not found (install librsvg)")
    version = subprocess.run([BIN, "--version"], capture_output=True, text=True, check=True).stdout.split()[-1]
    os.makedirs(OUT, exist_ok=True)
    magick = shutil.which("magick") or shutil.which("convert")
    with tempfile.TemporaryDirectory() as tmp:
        make_corpus(tmp)
        for name, (extra, steps) in SCENES.items():
            screen = shoot(tmp, os.path.join(tmp, "state"), extra, steps)
            svg, png = os.path.join(tmp, name + ".svg"), os.path.join(tmp, name + ".png")
            to_svg(screen, svg)
            subprocess.run(["rsvg-convert", "-z", "1.5", svg, "-o", png], check=True)
            dest = os.path.join(OUT, f"tui-{name}.png")
            if magick:
                subprocess.run([magick, png, "-strip", "PNG8:" + dest], check=True, stderr=subprocess.DEVNULL)
            else:
                shutil.copy(png, dest)
            print(f"wrote {os.path.relpath(dest, ROOT)}")
    with open(os.path.join(OUT, "VERSION"), "w") as f:
        f.write(version + "\n")
    print(f"screenshots are for version {version}")


if __name__ == "__main__":
    main()
