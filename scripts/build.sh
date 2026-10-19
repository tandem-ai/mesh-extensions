#!/usr/bin/env bash
# build.sh <extension> — builds providers/<extension> (or extensions/<extension>)
# to WASM and packs it into dist/<name>-<version>.meshext with `mesh ext pack`.
# MESH_SIGN_KEY=<key file> signs it (mesh/ extensions load only signed by mesh).
# MESH=<path> picks the mesh binary (default: mesh on PATH).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
name=${1:?usage: build.sh <extension>}
src=$root/providers/$name
[ -d "$src" ] || src=$root/extensions/$name
[ -f "$src/manifest.toml" ] || { echo "build.sh: no $name" >&2; exit 2; }
crate=$(sed -n 's/^name = "\(.*\)"/\1/p' "$src/Cargo.toml" | head -1)
(cd "$root" && cargo build -q --release --target wasm32-unknown-unknown -p "$crate")
stage=$root/dist/$name
rm -rf "$stage"; mkdir -p "$stage"
cp "$src/manifest.toml" "$stage/"
cp "$root/target/wasm32-unknown-unknown/release/${crate//-/_}.wasm" "$stage/extension.wasm"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$src/manifest.toml" | head -1)
sign=()
[ -n "${MESH_SIGN_KEY:-}" ] && sign=(--sign-key "$MESH_SIGN_KEY")
"${MESH:-mesh}" ext pack "$stage" -o "$root/dist/$name-$version.meshext" ${sign[@]+"${sign[@]}"}
