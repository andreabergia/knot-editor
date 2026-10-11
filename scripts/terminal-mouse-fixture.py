#!/usr/bin/env python3
"""Interactive mouse-report fixture for the terminal compatibility contract."""

import argparse
import os
import re
import sys
import termios
import tty


def packet(buffer, encoding):
    """Return (consumed, report) or None while awaiting a complete packet."""
    if buffer.startswith(b"\x1b[<"):
        match = re.match(rb"\x1b\[<(\d+);(\d+);(\d+)([Mm])", buffer)
        if match is None:
            return None
        code, column, row = map(int, match.group(1, 2, 3))
        return match.end(), (code, column, row, match.group(4) == b"m")
    if buffer.startswith(b"\x1b[M"):
        if encoding == "utf8":
            try:
                text = buffer[3:].decode("utf-8")
            except UnicodeDecodeError as error:
                if error.reason == "unexpected end of data":
                    return None
                return 1, None
            if len(text) < 3:
                return None
            values = list(map(ord, text[:3]))
            consumed = 3 + len(text[:3].encode("utf-8"))
        else:
            if len(buffer) < 6:
                return None
            values, consumed = buffer[3:6], 6
        code, column, row = (value - 32 for value in values)
        release = code & 3 == 3 and not code & (32 | 64)
        return consumed, (code, column, row, release)
    if b"\x1b[<".startswith(buffer) or b"\x1b[M".startswith(buffer):
        return None
    return 1, None


def output(text):
    os.write(sys.stdout.fileno(), text.encode("utf-8"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tracking", choices=["clicks", "drag", "motion"], default="drag")
    parser.add_argument("--encoding", choices=["legacy", "utf8", "sgr"], default="sgr")
    parser.add_argument("--alternate", action="store_true")
    args = parser.parse_args()
    if not sys.stdin.isatty() or not sys.stdout.isatty():
        parser.error("run this fixture inside an interactive terminal")
    tracking = {"clicks": 1000, "drag": 1002, "motion": 1003}[args.tracking]
    original = termios.tcgetattr(sys.stdin.fileno())
    buffer = b""
    count = 0
    try:
        tty.setraw(sys.stdin.fileno())
        if args.alternate:
            output("\x1b[?1049h")
        output("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l")
        output(f"\x1b[?{tracking}h")
        if args.encoding != "legacy":
            output(f"\x1b[?{1005 if args.encoding == 'utf8' else 1006}h")
        output(f"Mouse fixture: {args.tracking}, {args.encoding}; q quits.\r\n")
        output("Click/drag/wheel here. Hold Shift before pressing for local selection; Shift-wheel scrolls locally.\r\n")
        output("Wide/combining sample: A界e\u0301Z | cells are reported from 1,1.\r\n")
        while True:
            data = os.read(sys.stdin.fileno(), 4096)
            if not data:
                return
            buffer += data
            while buffer:
                decoded = packet(buffer, args.encoding)
                if decoded is None:
                    break
                consumed, report = decoded
                raw, buffer = buffer[:consumed], buffer[consumed:]
                if report is None:
                    if raw in (b"q", b"\x03"):
                        return
                    continue
                code, column, row, release = report
                if code & 64 and not code & 128:
                    action = ["wheel-up", "wheel-down", "wheel-left", "wheel-right"][code & 3]
                else:
                    action = "release" if release else "motion" if code & 32 else "press"
                count += 1
                output(f"{count}: {action} code={code} cell={column},{row} bytes={raw.hex(' ')}\r\n")
    finally:
        output("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l")
        if args.alternate:
            output("\x1b[?1049l")
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, original)


if __name__ == "__main__":
    main()
