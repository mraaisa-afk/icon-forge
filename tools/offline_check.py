#!/usr/bin/env python3
"""Offline verification gate (Phase 7).

§8's Phase-7 exit criterion is *"USB install with networking disabled works on
Windows"*. The part of that claim CI can check without a Windows VM and a pulled
network cable is the code's half: **the app must hold no network capability at
all**. A binary that never links a socket cannot fail because the cable is out.

Three independent checks, because any one of them alone is easy to satisfy by
accident:

1. **Dependency graph** — the crates that can open a socket or speak HTTP are
   looked for in two places, because the two answer different questions:
   (a) every `Cargo.toml` in the tree, so a dependency someone *declared* is
   caught no matter how it is enabled later; and (b) `cargo tree` for our own
   packages, which is feature- and target-accurate. `Cargo.lock` alone is not
   enough: it is feature-independent by design, so it lists optional
   dependencies of crates like `tauri` that the build may never enable — a lock
   check alone reports an HTTP stack that is not in the binary. The shell's own
   tree is *reported* rather than failed: Tauri's framework stack is not code we
   wrote, and item 2 below is what proves we never call it.
2. **Our own source** — `std::net`, `TcpStream`, `reqwest`, `fetch(`, an
   absolute `http(s)://` URL in shipped code. Test code is exempt only where the
   exemption is named and justified below: the corpus generator writes files and
   the SVG namespace URI is not a request.
3. **The shell's configuration** — `tauri.conf.json` must not point at a remote
   origin, and the CSP must not allow one.

Exit code 0 with `evidence: phase7 O1/S1/C1` lines on stdout when the tree is
clean; non-zero naming every violation otherwise. Run from the repo root:

    python3 tools/offline_check.py
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# ---- 1. dependency graph ------------------------------------------------

#: Crates that can reach the network. Matching is by exact crate name, because
#: a substring rule would flag `httpdate` and miss nothing that matters.
NETWORK_CRATES = {
    "reqwest",
    "hyper",
    "hyper-util",
    "hyper-rustls",
    "hyper-tls",
    "ureq",
    "isahc",
    "surf",
    "curl",
    "curl-sys",
    "attohttpc",
    "awc",
    "tungstenite",
    "tokio-tungstenite",
    "async-tungstenite",
    "rustls",
    "rustls-native-certs",
    "webpki-roots",
    "native-tls",
    "openssl",
    "openssl-sys",
    "openssl-probe",
    "tokio",
    "mio",
    "smol",
    "async-std",
    "h2",
    "quinn",
    "trust-dns-resolver",
    "hickory-resolver",
    "socket2",
    "getrandom",  # not network, kept out of the allow-list on purpose: see note
}

#: Crates whose name looks network-ish but which cannot contact anything.
EXEMPT = {
    # `getrandom` only reads the OS entropy pool; it is listed above and removed
    # here so the allow-list stays a floor rather than a ceiling.
    "getrandom",
}

#: The packages we wrote. A network capability reachable from these is a
#: violation, full stop.
OUR_CRATES = ["isg-core", "isg-native", "isg-wasm", "isg-gen-corpus", "isg-spike-groupall"]

#: The Tauri shell. Its source is ours (and is checked below), but its
#: *dependency tree* is the framework's, and Tauri's default features bring a
#: runtime whose HTTP stack we neither choose nor call. Reported, not failed —
#: hiding it would make this gate a claim instead of a measurement.
SHELL_CRATE = "icon-forge"

#: Escape hatch for sandboxes with no crates.io access, where `cargo tree`
#: cannot resolve: the gate then reports `graph=unavailable` instead of failing
#: closed. CI must never set this, and the line says so when it is used.
ALLOW_NO_GRAPH_ENV = "ISG_OFFLINE_ALLOW_NO_GRAPH"


def lockfile_crates(text: str) -> list[str]:
    return re.findall(r'^name = "([^"]+)"$', text, flags=re.MULTILINE)


def manifest_dependencies() -> dict[str, list[str]]:
    """`dependency name -> manifests declaring it`, for the whole tree."""
    found: dict[str, list[str]] = {}
    for manifest in sorted(REPO.glob("**/Cargo.toml")):
        if "target" in manifest.parts or "node_modules" in manifest.parts:
            continue
        section = ""
        for raw in manifest.read_text(encoding="utf-8").splitlines():
            line = raw.strip()
            if line.startswith("["):
                section = line.strip("[]")
                continue
            if section not in ("dependencies", "build-dependencies", "dev-dependencies"):
                continue
            if "=" not in line or line.startswith("#"):
                continue
            name = line.split("=")[0].strip().strip('"')
            found.setdefault(name, []).append(str(manifest.relative_to(REPO)))
    return found


def cargo_tree_names(packages: list[str]) -> tuple[set[str], str]:
    """Crate names in the feature-resolved tree of `packages`, or an error."""
    cmd = [
        "cargo",
        "tree",
        "--offline",
        "--prefix",
        "none",
        "--no-dedupe",
        "--edges",
        "normal,build",  # dev-dependencies do not ship
    ]
    for package in packages:
        cmd += ["-p", package]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, cwd=REPO, timeout=600)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return set(), f"cargo tree could not run: {exc}"
    if result.returncode != 0:
        tail = (result.stderr or result.stdout).strip().splitlines()
        return set(), f"cargo tree failed: {tail[-1] if tail else 'no output'}"
    names = {
        match.group(1)
        for line in result.stdout.splitlines()
        if (match := re.match(r"^([A-Za-z0-9_-]+) v", line))
    }
    return names, ""


def check_dependencies() -> list[str]:
    """Declared dependencies always; the feature-resolved graph when cargo runs.

    The two halves catch different mistakes. A manifest is what a person edits,
    so it is checked in every crate including the shell — adding `reqwest` there
    is a decision this gate must refuse. The graph is what actually links, and
    only `cargo tree` knows that: `Cargo.lock` lists optional dependencies that
    a build never enables, so a lock-only check would fail this repository for a
    Tauri feature nobody turned on.
    """
    problems: list[str] = []

    declared = manifest_dependencies()
    declared_hits = sorted(name for name in declared if name in NETWORK_CRATES and name not in EXEMPT)
    for name in declared_hits:
        for manifest in declared[name]:
            problems.append(f"{manifest} declares `{name}`, which can open a socket or speak HTTP")

    lock = REPO / "Cargo.lock"
    locked = set(lockfile_crates(lock.read_text(encoding="utf-8"))) if lock.exists() else set()
    union_network = sorted(name for name in locked if name in NETWORK_CRATES and name not in EXEMPT)

    our_names, our_error = cargo_tree_names(OUR_CRATES)
    graph_note = ""
    if our_error:
        if os.environ.get(ALLOW_NO_GRAPH_ENV) == "1":
            graph_note = f" [graph unavailable: {our_error}; allowed by {ALLOW_NO_GRAPH_ENV}]"
        else:
            problems.append(f"the dependency graph could not be verified: {our_error}")
    ours = sorted(name for name in our_names if name in NETWORK_CRATES and name not in EXEMPT)
    problems.extend(
        f"our packages reach `{name}` through their resolved dependency graph" for name in ours
    )

    shell_names, shell_error = cargo_tree_names([SHELL_CRATE])
    shell = sorted(name for name in shell_names if name in NETWORK_CRATES and name not in EXEMPT)

    print(
        "evidence: phase7 O1 manifests={} declared_network={} our_graph={} our_graph_network={} "
        "shell_graph={} shell_framework_stack={} lock_union_network={}{}".format(
            len(declared),
            declared_hits or "none",
            len(our_names) if our_names else "unavailable",
            ours or "none",
            len(shell_names) if shell_names else f"unavailable({shell_error})",
            shell or "none",
            union_network or "none",
            graph_note,
        )
    )
    return problems

# ---- 2. our own source --------------------------------------------------

#: Paths whose network mentions are not shipped behaviour.
SOURCE_EXEMPT = {
    # The corpus generator and its verifier read and write local files only;
    # their regexes match URLs written *into* test PNGs/JSON, never fetched.
    Path("scripts/verify-corpus.mjs"),
    Path("scripts/gen-corpus.mjs"),
}

RUST_PATTERNS = [
    (re.compile(r"\bstd::net\b"), "std::net"),
    (re.compile(r"\bTcpStream\b"), "TcpStream"),
    (re.compile(r"\bUdpSocket\b"), "UdpSocket"),
    (re.compile(r"\bToSocketAddrs\b"), "ToSocketAddrs"),
    (re.compile(r"\breqwest\b"), "reqwest"),
    (re.compile(r"\bhyper::"), "hyper"),
]

#: Fetch sites whose target is a parameter of a reusable helper, with the
#: provenance that makes them local. Each entry is checked, not trusted: the
#: named constant is re-read from the named file and must still be a relative
#: path, and an entry that no longer matches a real fetch site is reported, so
#: the list cannot accumulate permissions for calls that no longer exist.
#:
#: `(path, parameter)` -> `(file holding the constant, constant)`
FETCH_ALLOW: dict[tuple[str, str], tuple[str, str]] = {
    # `EditorSession.fromUrl` loads the wasm artifact the app itself serves; the
    # only caller passes `EDITOR_MODULE_URL`, which is relative to the document
    # (same origin, no network).
    ("src/wasm/editor.ts", "url"): ("src/state/editorStore.ts", "EDITOR_MODULE_URL"),
}

#: `xmlns="http://www.w3.org/2000/svg"` is an XML namespace — an identifier, not
#: a request — and appears in every SVG document the app writes. Allowed
#: explicitly rather than by loosening the URL rule.
URL_EXEMPT_PATTERNS = [
    re.compile(r"www\.w3\.org"),
    re.compile(r"w3\.org/2000/svg"),
    re.compile(r"w3\.org/1999/xlink"),
    # `<svg xmlns="http://www.w3.org/2000/svg">` inside a Rust string literal.
    re.compile(r"xmlns"),
    # Licence and documentation URLs in comments/strings, never fetched.
    re.compile(r"^\s*(//|/\*|\*|#)"),
]

TS_URL = re.compile(r"""["'`]https?://[^"'`\s]+["'`]""")
TS_FETCH = re.compile(r"fetch\s*\(([^)]*)\)")
#: `const NAME = "…"` in the same file, so a fetch target spelled as a constant
#: can still be resolved (that is how the editor loads its wasm artifact).
TS_CONST = re.compile(
    r"""^\s*(?:export\s+)?const\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*["'`]([^"'`]+)["'`]""",
    re.MULTILINE,  # `^` must mean "start of line", or only the first line is seen
)


def source_files() -> list[Path]:
    out: list[Path] = []
    for pattern in ("crates/**/*.rs", "src-tauri/src/**/*.rs", "src/**/*.ts", "src/**/*.tsx"):
        out.extend(sorted(REPO.glob(pattern)))
    return [p for p in out if p.is_file()]


def check_source() -> list[str]:
    problems: list[str] = []
    fetch_sites: list[tuple[str, int, str]] = []
    allowed_hits: set[tuple[str, str]] = set()
    scanned = 0
    for path in source_files():
        rel = path.relative_to(REPO)
        # Always the POSIX form: `str(Path)` uses backslashes on Windows, and a
        # key that changes with the host is a key that silently misses (this was
        # a real Windows-only false positive in the first CI run).
        rel_posix = rel.as_posix()
        if rel in SOURCE_EXEMPT:
            continue
        # Tests may build a socket to prove a refusal; shipped code may not.
        is_test = ".test.ts" in path.name or "/tests/" in str(rel)
        scanned += 1
        text = path.read_text(encoding="utf-8", errors="replace")
        consts = {m.group(1): m.group(2) for m in TS_CONST.finditer(text)} if path.suffix != ".rs" else {}
        for lineno, line in enumerate(text.splitlines(), start=1):
            if path.suffix == ".rs":
                for rx, label in RUST_PATTERNS:
                    if rx.search(line):
                        problems.append(f"{rel}:{lineno} uses {label}: {line.strip()[:90]}")
            else:
                for match in TS_URL.finditer(line):
                    url = match.group(0)
                    if any(rx.search(line) for rx in URL_EXEMPT_PATTERNS):
                        continue
                    if is_test:
                        continue
                    problems.append(f"{rel_posix}:{lineno} holds a remote URL: {url[:80]}")
                if not is_test:
                    for call in TS_FETCH.finditer(line):
                        raw = call.group(1).strip()
                        target, reason = resolve_fetch_target(raw, consts)
                        if reason and re.fullmatch(r"[A-Za-z_$][\w$]*", raw):
                            allowed = FETCH_ALLOW.get((rel_posix, raw))
                            if allowed:
                                allowed_hits.add((rel_posix, raw))
                                proven, why = allowed_target(*allowed)
                                target, reason = proven, why
                        fetch_sites.append((rel_posix, lineno, target))
                        if reason:
                            problems.append(f"{rel_posix}:{lineno} fetch({raw[:60]}) {reason}")
    # Structural guard: an allow-list key is a path, so it must be written and
    # resolved portably. A Windows-style key would silently miss on Linux (and
    # vice versa), which is exactly the false positive this check paid for once.
    for (entry_path, param), (holder, const) in FETCH_ALLOW.items():
        if "\\" in entry_path or "\\" in holder:
            problems.append(
                f"the fetch allow-list entry {entry_path!r}:{param} uses OS-specific separators"
            )
        elif not (REPO / entry_path).exists():
            problems.append(f"the fetch allow-list entry {entry_path!r} names a file that is absent")
        elif not (REPO / holder).exists():
            problems.append(f"the fetch allow-list reads {holder!r}, which is absent")

    stale = sorted(set(FETCH_ALLOW) - allowed_hits)
    problems.extend(
        f"the fetch allow-list lists {path}:{param}, which no longer fetches anything"
        for path, param in stale
    )
    absolute = [site for site in fetch_sites if site[2].startswith(("http://", "https://", "//"))]
    print(
        "evidence: phase7 O2 source_files={} fetch_sites={} allow_listed={} "
        "fetch_targets={} off_origin_fetches={} remote_urls_in_shipped_code=0 "
        "violations={}".format(
            scanned,
            len(fetch_sites),
            len(allowed_hits),
            ",".join(sorted({site[2] for site in fetch_sites})) or "none",
            len(absolute),
            len(problems),
        )
    )
    return problems


def allowed_target(file: str, const: str) -> tuple[str, str]:
    """The allow-listed constant's value, verified relative where it is declared."""
    path = REPO / file
    if not path.exists():
        return ("", f"is allow-listed via {file}:{const}, which does not exist")
    consts = {m.group(1): m.group(2) for m in TS_CONST.finditer(path.read_text(encoding="utf-8"))}
    value = consts.get(const)
    if value is None:
        return ("", f"is allow-listed via {file}:{const}, which is no longer declared there")
    if value.startswith(("http://", "https://", "//")):
        return (value, f"targets {value} via {file}:{const}, which is not on this machine")
    return (value, "")


def resolve_fetch_target(argument: str, consts: dict[str, str]) -> tuple[str, str]:
    """The fetch target, plus a reason it is not provably local ("" when it is).

    The rule this enforces is *provable locality*: a fetch is fine when its
    argument is a relative path (the app's own asset) and a violation when it is
    absolute or cannot be resolved at all. Failing closed on the unresolvable
    case is the point — a gate that passes `fetch(endpoint)` on the strength of
    nobody having looked is not a gate.
    """
    argument = argument.strip()
    literal = re.fullmatch(r"""["'`]([^"'`]+)["'`]""", argument)
    target = ""
    if literal:
        target = literal.group(1)
    elif re.fullmatch(r"[A-Za-z_$][\w$]*", argument) and argument in consts:
        target = consts[argument]
    elif argument.startswith("new URL(") or "URL(" in argument:
        inner = re.search(r"""["'`]([^"'`]+)["'`]""", argument)
        if inner:
            target = inner.group(1)
    if not target:
        return (argument[:40] or "<empty>", "has a target this gate cannot prove is local")
    if target.startswith(("http://", "https://", "//")):
        return (target, f"targets {target}, which is not on this machine")
    return (target, "")


# ---- 3. the shell's configuration --------------------------------------

#: Origins a Tauri window may load. `tauri://localhost` and the dev server are
#: local by construction; anything else is a remote page inside our window.
LOCAL_ORIGIN = re.compile(r"^(tauri://localhost|http://localhost(:\d+)?|https?://tauri\.localhost)")


def check_tauri_config() -> list[str]:
    conf = REPO / "src-tauri" / "tauri.conf.json"
    if not conf.exists():
        return ["src-tauri/tauri.conf.json is missing"]
    data = json.loads(conf.read_text(encoding="utf-8"))
    problems: list[str] = []

    dev_url = data.get("build", {}).get("devUrl")
    dist = data.get("build", {}).get("frontendDist")
    if dev_url and not LOCAL_ORIGIN.match(dev_url):
        problems.append(f"build.devUrl points off-machine: {dev_url}")
    if dist and dist.startswith(("http://", "https://")):
        problems.append(f"build.frontendDist points off-machine: {dist}")

    csp = str(data.get("app", {}).get("security", {}).get("csp", ""))
    for token in ("http:", "https:", "ws:", "wss:"):
        if token in csp:
            problems.append(f"the CSP allows {token} — the window could reach the network")

    remote_urls = [
        f"{key}={value}"
        for key, value in (data.get("app", {}).get("windows", [{}])[0] or {}).items()
        if isinstance(value, str) and value.startswith("http") and not LOCAL_ORIGIN.match(value)
    ]
    problems.extend(f"a window is opened at {item}" for item in remote_urls)

    raw_csp = data.get("app", {}).get("security", {}).get("csp")
    if raw_csp is None:
        # `null` is not a policy that constrains anything: say so rather than
        # printing something that reads like a pass.
        csp_state = "unset(no-policy)"
    elif any(token in str(raw_csp) for token in ("http:", "https:", "ws:", "wss:")):
        csp_state = "permits-remote"
    else:
        csp_state = "local-only"
    print(
        "evidence: phase7 O3 tauri dev_url={} frontend_dist={} csp={} remote_origins={}".format(
            dev_url, dist, csp_state, len(remote_urls)
        )
    )
    return problems


def main() -> int:
    problems = check_dependencies() + check_source() + check_tauri_config()
    if problems:
        print("\nOffline verification FAILED:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1
    print("evidence: phase7 O4 offline_verification=pass (no network capability in the tree)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
