#!/usr/bin/env bash
# Toolchain bootstrap for network-isolated development sandboxes.
#
# crates.io, static.crates.io and sh.rustup.rs are unreachable from some
# environments where this repo is worked on (see the note in the workspace
# `Cargo.toml` about offline development). registry.npmjs.org is not, and the
# `@rustbin` packages carry a whole toolchain: rustc, cargo, rustfmt, clippy and
# one target's std each.
#
# Two traps, both hit in practice:
#   * the package name carries BOTH the version and the target
#     (`@rustbin/rustc-1.88.0-x86_64-unknown-linux-gnu`); the bare
#     `@rustbin/rustc` that looks obvious 404s;
#   * `npm pack` writes the tarball under the *scoped* name
#     (`rustbin-rustc-…-1.88.0.tgz`), and every tarball unpacks into the SAME
#     `package/` root — one directory per component — so the trees merge into
#     one prefix as they are extracted.
#
# Idempotent: a completed install leaves a stamp and this script then only
# re-exports PATH/LD_LIBRARY_PATH. Source it, then `cd` where you need to be —
# it changes directory at the end.
#
#   source scripts/setup-rust-offline.sh
#   cargo test -p isg-native --release
#
set -u

VERSION="${RUST_VERSION:-1.88.0}"
HOST="x86_64-unknown-linux-gnu"
TARGETS=("$HOST" "wasm32-unknown-unknown")
BUILD="${RUST_BUILD_DIR:-$HOME/.rust-build}"
ROOT="$BUILD/toolchain"
STAMP="$BUILD/.version"

export PATH="$ROOT/bin:$PATH"
export LD_LIBRARY_PATH="$ROOT/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

fetch() { # fetch <component> <target>
  local pkg="$1" target="$2" file
  file=$(npm pack "@rustbin/${pkg}-${VERSION}-${target}" --silent 2>/dev/null | tail -1)
  if [ -z "$file" ] || [ ! -f "$file" ]; then
    echo "[setup-rust] missing @rustbin/${pkg}-${VERSION}-${target}" >&2
    return 1
  fi
  tar xzf "$file"
}

if [ ! -f "$STAMP" ] || [ "$(cat "$STAMP" 2>/dev/null)" != "$VERSION" ]; then
  echo "[setup-rust] installing rust $VERSION into $ROOT"
  rm -rf "$ROOT" "$BUILD/pkgs"
  mkdir -p "$BUILD/pkgs" "$ROOT/bin" "$ROOT/lib"
  cd "$BUILD/pkgs" || return 1

  ok=1
  for comp in rustc cargo rustfmt clippy; do
    fetch "$comp" "$HOST" || ok=0
  done
  for target in "${TARGETS[@]}"; do
    fetch rust-std "$target" || ok=0
  done
  if [ "$ok" != 1 ]; then
    echo "[setup-rust] install incomplete; not stamping" >&2
    return 1
  fi

  # Hoist each component's bin/ and lib/ into one rustc prefix.
  for comp in "$BUILD"/pkgs/package/*/; do
    [ -d "$comp/bin" ] && cp -f "$comp"/bin/* "$ROOT/bin/" 2>/dev/null
    [ -d "$comp/lib" ] && cp -rf "$comp"/lib/* "$ROOT/lib/" 2>/dev/null
  done
  # `rustfmt` and `clippy-driver` ship as their own components and are invoked
  # as `cargo fmt` / `cargo clippy`.
  [ -f "$ROOT/bin/rustfmt" ] && ln -sf rustfmt "$ROOT/bin/cargo-fmt"
  [ -f "$ROOT/bin/clippy-driver" ] && ln -sf clippy-driver "$ROOT/bin/cargo-clippy"
  chmod +x "$ROOT"/bin/* 2>/dev/null
  if [ ! -x "$ROOT/bin/rustc" ]; then
    echo "[setup-rust] rustc did not land in $ROOT/bin" >&2
    return 1
  fi
  echo "$VERSION" > "$STAMP"
  echo "[setup-rust] installed $VERSION"
else
  echo "[setup-rust] $VERSION already installed"
fi

cd "$BUILD" || return 1
