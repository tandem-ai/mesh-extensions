# mesh-extensions

Extensions for [mesh](https://usetandem.com): what the community and the mesh team
build on top of it, through the host API only. The design is `docs/extensions.md` in
the mesh repo (decisions E1–E27).

| Path | What |
|---|---|
| `sdk/rust` | `mesh-ext`, the Rust SDK: host API 1.3 types and calls, over Extism |
| `providers/claude-code` | `mesh/claude-code`: connects Claude Code to mesh (hooks, the `mesh` and `mesh-ext` MCP servers, permissions) |
| `providers/codex` | `mesh/codex`: Codex (hooks and their trust, the `mesh` and `mesh-ext` MCP servers, in `config.toml`) |
| `providers/gemini` | `mesh/gemini`: Gemini CLI (hooks and the `mesh` and `mesh-ext` MCP servers, in `settings.json`) |
| `providers/cursor` | `mesh/cursor`: Cursor (hooks, the `mesh` and `mesh-ext` MCP servers, CLI permissions) |
| `providers/pi` | `mesh/pi`: Pi (a TypeScript extension) |
| `providers/hermes` | `mesh/hermes`: Hermes (a Python plugin, enabled with `hermes plugins`) |
| `extensions/pulse` | `mesh/pulse`: an example org extension (a server part with cron and a webhook, the store, an agent tool) |
| `scripts/build.sh` | builds an extension to WASM and packs it into a `.meshext` |
| `scripts/registry.sh` | builds them all and publishes them into `dist/registry`, the folder lookup serves at `/dl/extensions/` |

An extension is a WebAssembly module exporting `handle`, which gets one event (JSON) and
answers JSON. It reaches mesh through one import, `mesh_call`, behind the permissions in
its `manifest.toml`. A provider (`kind = "provider"`) also gets the process door: meshd
reads and writes the agent config files it declares, and runs the commands it declares,
for it. Claude Code, Codex and Pi also answer `launch` (host API 1.3): a cloud agent's
run (`mesh run --token`) gets mesh's tools as arguments, with files only in the run's
folder (`{run_dir}/` in the door) and nothing in `~`.

```sh
scripts/build.sh claude-code                  # dist/claude-code-<version>.meshext
mesh ext install dist/claude-code-*.meshext
mesh install claude                           # through the extension

scripts/registry.sh                           # dist/registry: index.json + packages
MESH_EXT_INDEX=$PWD/dist/registry/index.json mesh up   # or the lookup server's registry:
mesh ext install mesh/codex                   #   by id, the newest version
mesh install                                  #   every agent found, providers on demand
```

`mesh/` extensions only load signed by mesh (`scripts/build.sh` signs with
`MESH_SIGN_KEY`; debug builds of mesh also trust `MESH_EXT_DEV_KEY`).

Getting an extension on mesh's official list: open a PR here. The mesh team reviews it by
hand; once it is on `main`, the mesh team runs mesh's Extensions workflow, which builds
what the registry doesn't have yet (an id at a new version), signs it and publishes it. A
published version never changes: bump `version` in `manifest.toml` and `Cargo.toml`.

Licence: Apache-2.0.
