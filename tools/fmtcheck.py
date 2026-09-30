#!/usr/bin/env python3
"""Count a format macro's positional placeholders against its arguments.

`rustfmt` parses a file but never expands it and `clippy` never sees a test
binary that failed to build, so the one slip a 1400-line gate test is prone to —
`12 positional arguments in format string, but there are 11 arguments` — costs a
whole CI cycle to discover. Run against one or more Rust files:

    python3 tools/fmtcheck.py crates/isg-native/tests/phase6_gate.rs

It reports every `assert!` / `assert_eq!` / `panic!` / `format!` / `write!` /
`eprintln!` / `println!` whose format string takes more positional placeholders
than the macro is given arguments (or fewer). Inline captures (`{name}`,
`{name:.3}`) resolve against variables in scope and are not counted, so a macro
that uses them legitimately will not be flagged.
"""

import re
import sys

MACRO = re.compile(
    r"\b(eprintln|println|print|panic|assert|assert_eq|assert_ne|format|write|writeln)!\s*\("
)


def skip_string(src, k):
    """`src[k]` is a `"`; return the index just past the closing quote."""
    k += 1
    while k < len(src):
        if src[k] == "\\":
            k += 2
            continue
        if src[k] == '"':
            return k + 1
        k += 1
    return k


def skip_comment(src, k):
    """`src[k]` starts a `//` or `/* */` comment; return the index just past it."""
    if src.startswith("/*", k):
        end = src.find("*/", k + 2)
        return len(src) if end == -1 else end + 2
    end = src.find("\n", k + 2)
    return len(src) if end == -1 else end + 1


def is_comment(src, k):
    return src.startswith("//", k) or src.startswith("/*", k)


def is_char_literal(src, k):
    """True when `src[k]` is a `'` that starts a char literal, not a lifetime."""
    if k + 1 >= len(src):
        return False
    if src[k + 1] == "\\":
        end = src.find("'", k + 2)
        return end != -1 and end - k <= 4
    return src[k + 2 : k + 3] == "'"


def skip_char(src, k):
    end = src.find("'", k + 1)
    return len(src) if end == -1 else end + 1


def split_args(body):
    """Split a macro's argument list on top-level commas.

    Comments are stripped first: a `//` line may hold a quote or an apostrophe
    (`// don't widen the bar`), and reading that as code splits an argument in
    half and reports a mismatch that is not there.
    """
    args, depth, cur, k = [], 0, "", 0
    while k < len(body):
        c = body[k]
        if is_comment(body, k):
            k = skip_comment(body, k)
            continue
        if c == '"':
            end = skip_string(body, k)
            cur += body[k:end]
            k = end
            continue
        if c == "'" and is_char_literal(body, k):
            end = skip_char(body, k)
            cur += body[k:end]
            k = end
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        if c == "," and depth == 0:
            args.append(cur)
            cur = ""
        else:
            cur += c
        k += 1
    if cur.strip():
        args.append(cur)
    return args


NAMED_ARG = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*\s*=(?!=)")


def positional_args(args):
    """Named arguments (`n = 3`) always trail the positional ones; drop them."""
    for i, arg in enumerate(args):
        if NAMED_ARG.match(arg.strip()):
            return args[:i]
    return args


def literal_of(arg):
    """Return the string-literal text of an argument, or None."""
    text = arg.strip()
    if not text.startswith('"'):
        return None
    end = skip_string(text, 0)
    return text[:end]


def positional(literal):
    """Count `{}` / `{:>5}` / `{0}` placeholders inside a format literal."""
    body = literal[1 : literal.rfind('"')]
    body = body.replace("{{", "").replace("}}", "")
    count = 0
    for inner in re.findall(r"\{([^{}]*)\}", body):
        # `{name}` and `{name:...}` name a variable in scope; `{}`, `{:...}` and
        # `{0}` are positional.
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*(:|$)", inner):
            continue
        count += 1
    return count


def all_literal_positions(args):
    return [i for i, a in enumerate(args) if literal_of(a) is not None]


def check(path):
    src = open(path, encoding="utf-8").read()
    checked = bad = 0
    for match in MACRO.finditer(src):
        start = match.end()
        depth, k = 1, start
        while k < len(src) and depth:
            c = src[k]
            if is_comment(src, k):
                k = skip_comment(src, k)
                continue
            if c == '"':
                k = skip_string(src, k)
                continue
            if c == "'" and is_char_literal(src, k):
                k = skip_char(src, k)
                continue
            if c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
            k += 1
        args = split_args(src[start : k - 1])
        # Where the format literal sits depends on the macro. In `format!` and
        # friends it is the first argument; in `assert!`-style macros and
        # `write!` it is an argument whose position depends on how many values
        # precede it — and a *value* can itself be a string literal
        # (`assert_eq!(state, "flag", "…")`), so the literal position cannot be
        # guessed from the text alone. What can be checked is consistency: there
        # must exist some literal argument whose placeholder count equals the
        # number of arguments after it.
        args = positional_args(args)
        literal_at = all_literal_positions(args)
        if not literal_at:
            continue
        checked += 1
        for index in literal_at:
            literal = literal_of(args[index])
            wanted = positional(literal)
            given = len(args) - index - 1
            if wanted == given:
                break
        else:
            index = literal_at[0]
            literal = literal_of(args[index])
            wanted = positional(literal)
            given = len(args) - index - 1
            line = src[: match.start()].count("\n") + 1
            print(f"{path}:{line}: {match.group(1)}!: {wanted} placeholders, {given} args")
            print(f"    {literal[:100]}")
            print(f"    args: {[a.strip()[:38] for a in args[index + 1 :]]}")
            bad += 1
    return checked, bad


def main(paths):
    checked = bad = 0
    for path in paths:
        c, b = check(path)
        checked += c
        bad += b
    print(f"checked {checked} format macros in {len(paths)} file(s), {bad} mismatch(es)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:] or ["crates/isg-native/tests/phase6_gate.rs"]))
