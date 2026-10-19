#!/usr/bin/env bash
# index.sh [extension…] — builds the extensions (default: every provider) with
# build.sh, then writes dist/index.json listing their packages, the index
# `mesh ext install <id>` and install on demand read (MESH_EXT_INDEX).
# URL_PREFIX=<url/> puts the packages' URLs under it (default: next to the index).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for d in "$root"/providers/*/; do names+=("$(basename "$d")"); done
fi
packages=()
for n in "${names[@]}"; do
  "$root/scripts/build.sh" "$n" >/dev/null
  src=$root/providers/$n; [ -d "$src" ] || src=$root/extensions/$n
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$src/manifest.toml" | head -1)
  packages+=("$root/dist/$n-$version.meshext")
done
"${MESH:-mesh}" ext index "${packages[@]}" --url-prefix "${URL_PREFIX:-}" -o "$root/dist/index.json"
