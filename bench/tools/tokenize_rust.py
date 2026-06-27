#!/usr/bin/env python3
"""Generate a `.kfx` fixture from a Rust source file.

This is a throwaway codegen tool, NOT part of the bench runtime. The
plan forbids pulling a parser into the harness; the version-controlled
`.kfx` output is the artifact the bench loads. The tokenizer is a
coarse regex approximation — good enough to exercise multi-attribute
shaping, not a real syntax highlighter.

Usage:
    tokenize_rust.py <input.rs> <output.kfx>

Color scheme (0xRRGGBB), matching Token::color in rust_sample.rs:
    keyword     0x569CD6  bold
    type         0x4EC9B0
    function     0xDCDCAA
    macro        0xC586C0  bold
    string       0xCE9178
    number       0xB5CEA8
    comment      0x6A9955  italic
    lifetime     0xD7BA7D
    attribute    0xC586C0  bold
    punctuation  0xD4D4D4
    ident        0x9CDCFE
    other/plain  0xC0C0C0
"""

import re
import sys

KEYWORDS = {
    "as", "async", "await", "break", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
    "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "yield",
}

BUILTIN_TYPES = {
    "bool", "char", "str", "u8", "u16", "u32", "u64", "usize",
    "i8", "i16", "i32", "i64", "isize", "f32", "f64",
    "String", "Vec", "Option", "Result", "Box", "Rc", "Arc",
    "Instant", "Duration", "Range",
}

# Token regex, ordered by priority. Each pattern matches a contiguous
# run of one kind; whitespace and other chars fall through to "plain".
TOKEN_RE = re.compile(
    r"""
    (?P<comment>      //[^\n]* | /\*.*?\*/          )
  | (?P<attribute>     \#!?\[[^\]]*\]                )
  | (?P<string>       "(?:\\.|[^"\\])*"            )
  | (?P<rawstring>     r"(?:\\.|[^"\\])*"           )
  | (?P<charlit>       '(?:\\.|[^'\\])'             )
  | (?P<lifetime>      '\w+                         )
  | (?P<number>        \d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?(?:f32|f64|u8|u16|u32|u64|usize|i8|i16|i32|i64|isize)?
                       | 0x[0-9a-fA-F_]+
                       | 0b[01_]+
                       | 0o[0-7_]+                  )
  | (?P<macro>         [A-Za-z_]\w*!                )
  | (?P<ident>         [A-Za-z_]\w*                 )
  | (?P<punct>         [{}\[\]()<>,;:.+=\-*/%&|^!~?]+ )
    """,
    re.VERBOSE | re.DOTALL,
)

COLORS = {
    "keyword":    (0x569CD6, True,  False),
    "type":       (0x4EC9B0, False, False),
    "function":   (0xDCDCAA, False, False),
    "macro":      (0xC586C0, True,  False),
    "string":     (0xCE9178, False, False),
    "number":     (0xB5CEA8, False, False),
    "comment":    (0x6A9955, False, True),
    "lifetime":   (0xD7BA7D, False, False),
    "attribute":  (0xC586C0, True,  False),
    "punctuation":(0xD4D4D4, False, False),
    "ident":      (0x9CDCFE, False, False),
    "plain":      (0xC0C0C0, False, False),
}


def classify_token(kind, text):
    """Refine an `ident` into keyword/type/function."""
    if kind == "ident":
        if text in KEYWORDS:
            return "keyword"
        if text in BUILTIN_TYPES:
            return "type"
        # Heuristic: Capitalized identifiers or those ending in `_t` are types;
        # identifiers followed by `(` on the same line are functions.
        if text[:1].isupper() or text.endswith("_t"):
            return "type"
        return "ident"
    if kind == "rawstring":
        return "string"
    if kind == "charlit":
        return "string"
    if kind == "punct":
        return "punctuation"
    return kind


def tokenize_line(line):
    """Yield (kind, start, end, text) tuples for one line."""
    pos = 0
    n = len(line)
    while pos < n:
        m = TOKEN_RE.match(line, pos)
        if not m:
            # Unrecognized char: emit one byte of plain.
            yield ("plain", pos, pos + 1, line[pos])
            pos += 1
            continue
        start = m.start()
        end = m.end()
        kind = m.lastgroup
        text = m.group()
        # Whitespace inside a punct/ident gap: split it out as plain.
        if kind == "punct" and re.search(r"\s", text):
            i = start
            for ws in re.finditer(r"\s+", text):
                if ws.start() > 0:
                    sub = text[: ws.start()]
                    yield ("punctuation", i, i + len(sub), sub)
                yield ("plain", i + ws.start(), i + ws.end(), ws.group())
                i = i + ws.end()
            if i < end:
                sub = text[i - start:]
                yield ("punctuation", i, end, sub)
        else:
            refined = classify_token(kind, text)
            yield (refined, start, end, text)
        pos = end


def function_heuristic(lines):
    """Mark identifiers immediately followed by `(` as functions."""
    func_ids = set()
    for line in lines:
        for m in re.finditer(r"([A-Za-z_]\w*)\s*\(", line):
            name = m.group(1)
            if name not in KEYWORDS and name not in BUILTIN_TYPES:
                func_ids.add(name)
    return func_ids


def main():
    if len(sys.argv) != 3:
        print("usage: tokenize_rust.py <input.rs> <output.kfx>", file=sys.stderr)
        sys.exit(2)

    in_path, out_path = sys.argv[1], sys.argv[2]
    with open(in_path, "r", encoding="utf-8") as f:
        content = f.read()

    lines = content.split("\n")
    # Drop a trailing empty line from the final newline, if any.
    if lines and lines[-1] == "":
        lines.pop()

    func_names = function_heuristic(lines)

    out = []
    out.append("knot-fixture v1")
    out.append(str(len(lines)))
    out.extend(lines)
    out.append("---")

    for idx, line in enumerate(lines):
        for kind, start, end, _text in tokenize_line(line):
            if kind == "ident" and _text in func_names:
                kind = "function"
            color, bold, italic = COLORS[kind]
            out.append(f"{idx} {start} {end} 0x{color:06X} {1 if bold else 0} {1 if italic else 0}")

    with open(out_path, "w", encoding="utf-8") as f:
        f.write("\n".join(out))
        f.write("\n")

    print(f"wrote {out_path}: {len(lines)} lines")


if __name__ == "__main__":
    main()
