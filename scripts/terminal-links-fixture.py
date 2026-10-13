#!/usr/bin/env python3
"""Print explicit and detected links for native terminal acceptance."""

print("Detected: https://example.com/ (https://example.com/a(b)).")
print("Unicode path: https://example.com/界e\u0301")
print("Wrapped: https://example.com/" + "wide-path/" * 16)
print("OSC 8: \033]8;id=fixture;https://example.com/\033\\click this label\033]8;;\033\\")
print("Unsupported OSC 8: \033]8;;file:///tmp/example\033\\https://example.com/\033]8;;\033\\")
print("Command-click on macOS / Control-click elsewhere; Shift selects locally.")
