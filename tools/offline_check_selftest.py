#!/usr/bin/env python3
"""Negative controls for `tools/offline_check.py` (Phase 7).

A verification gate that has never failed is not evidence of anything. This
script builds a throwaway copy of the tree, checks the gate passes it, then
re-introduces each capability the gate claims to catch — one at a time — and
requires the gate to reject every one of them for the stated reason.

    python3 tools/offline_check_selftest.py

Exits non-zero if any mutation survives the gate, or if the unmutated copy is
rejected (a gate that fails everything proves nothing either).

The gate's `cargo tree` half needs a resolvable registry; in a sandbox without
one, export `ISG_OFFLINE_ALLOW_NO_GRAPH=1` to let that half report itself as
unavailable instead of failing. CI never sets it.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

#: Directories copied into the throwaway tree: everything the gate reads, and
#: nothing it does not (`target/` is excluded: cargo writes build output there
#: mid-run and copying it would dominate the runtime).
SKIP = {".git", "target", "node_modules", "dist", "bench"}


def snapshot(dest: Path) -> None:
    for child in REPO.iterdir():
        if child.name in SKIP:
            continue
        if child.is_dir():
            shutil.copytree(child, dest / child.name, symlinks=True)
        else:
            shutil.copy2(child, dest / child.name)


def run_gate(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(root / "tools" / "offline_check.py")],
        capture_output=True,
        text=True,
        cwd=root,
    )


def append(dest: Path, name: str, text: str) -> None:
    path = dest / name
    path.write_text(path.read_text(encoding="utf-8") + text, encoding="utf-8")


def add_dependency(dest: Path) -> str:
    manifest = sorted((dest / "crates").glob("*/Cargo.toml"))[0]
    manifest.write_text(
        manifest.read_text(encoding="utf-8") + '\n[dependencies]\nreqwest = "0.12"\n',
        encoding="utf-8",
    )
    return f"{manifest.relative_to(dest)} gains a direct reqwest dependency"


def add_shell_dependency(dest: Path) -> str:
    """The shell is where a network capability would realistically creep in.

    It is checked through the manifests rather than the resolved graph: the
    graph of a Tauri shell legitimately contains the framework's runtime, and
    the lock file cannot tell an enabled feature from an optional one.
    """
    manifest = dest / "src-tauri" / "Cargo.toml"
    text = manifest.read_text(encoding="utf-8")
    patched = text.replace('[dependencies]\n', '[dependencies]\nreqwest = "0.12"\n', 1)
    assert patched != text, "could not add a dependency to the shell manifest"
    manifest.write_text(patched, encoding="utf-8")
    return "src-tauri/Cargo.toml declares an HTTP client"


def add_remote_fetch(dest: Path) -> str:
    target = dest / "src" / "lib" / "backend.ts"
    append(dest, "src/lib/backend.ts", '\nexport const ping = () => fetch("https://example.com/api");\n')
    return f"{target.relative_to(dest)} fetches a remote URL"


def add_socket_use(dest: Path) -> str:
    target = sorted((dest / "crates").glob("*/src/*.rs"))[0]
    append(dest, str(target.relative_to(dest)), "\nfn _probe() { let _ = std::net::TcpStream::connect;\n}\n")
    return f"{target.relative_to(dest)} names std::net"


def open_csp(dest: Path) -> str:
    path = dest / "src-tauri" / "tauri.conf.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    data["app"]["security"]["csp"] = "default-src 'self' https:"
    path.write_text(json.dumps(data, indent=2), encoding="utf-8")
    return "the CSP gains an https: source"


def retarget_allow_list(dest: Path) -> str:
    path = dest / "src" / "state" / "editorStore.ts"
    text = path.read_text(encoding="utf-8")
    patched = re.sub(
        r'EDITOR_MODULE_URL = "[^"]*"',
        'EDITOR_MODULE_URL = "https://cdn.example.com/isg_wasm.wasm"',
        text,
        count=1,
    )
    assert patched != text, "could not retarget EDITOR_MODULE_URL"
    path.write_text(patched, encoding="utf-8")
    return "the allow-listed editor artifact moves to a CDN"


MUTATIONS = [
    ("O1-direct-dependency", add_dependency, "socket or speak HTTP"),
    ("O1-shell-dependency", add_shell_dependency, "declares `reqwest`"),
    ("O2-remote-fetch", add_remote_fetch, "not on this machine"),
    ("O2-socket-api", add_socket_use, "std::net"),
    ("O3-permissive-csp", open_csp, "CSP allows https:"),
    ("O2-allow-list-provenance", retarget_allow_list, "not on this machine"),
]


def main() -> int:
    root = Path(tempfile.mkdtemp(prefix="offline-selftest-"))
    try:
        # The positive control: the real tree must pass, or every rejection
        # below would be evidence of a broken gate rather than a caught hole.
        clean = root / "clean"
        clean.mkdir()
        snapshot(clean)
        result = run_gate(clean)
        if result.returncode != 0:
            print("positive control FAILED — the gate rejects an unmodified tree:", file=sys.stderr)
            print(result.stdout, result.stderr, file=sys.stderr)
            return 1
        line = next(
            (l for l in result.stdout.splitlines() if "O4 offline_verification" in l),
            "(no verdict line)",
        )
        print(f"positive control ok — {line}")
        print(f"evidence: phase7 O5 negative_controls={len(MUTATIONS)} positive_control=pass")

        failures: list[str] = []
        for name, mutate, expected in MUTATIONS:
            tree = root / name
            tree.mkdir()
            snapshot(tree)
            described = mutate(tree)
            result = run_gate(tree)
            rejected = result.returncode != 0
            reason = expected in (result.stdout + result.stderr)
            verdict = "rejected" if rejected and reason else "SURVIVED"
            if verdict == "SURVIVED":
                failures.append(name)
            print(f"  {name}: {verdict} — {described}")

        if failures:
            print(
                "\nthe gate let these through: " + ", ".join(failures),
                file=sys.stderr,
            )
            return 1
        print(
            f"evidence: phase7 O6 all_{len(MUTATIONS)}_negative_controls=rejected "
            "for the expected reason"
        )
        return 0
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
