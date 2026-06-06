# commonsc-host

Tier-1 sandbox host. Spawns a Deno sidecar that loads Pyodide and runs Python
algorithm code; talks to it over a newline-delimited JSON bridge on stdin/stdout.
The Rust process is the only component with access to user data and the network;
plugin code runs inside the sidecar with no network access and no filesystem
access beyond the paths the host hands it.

This crate is build-order step 2 in `CommonSense/implementation-brief.md`. Its job
is to **prove the privacy boundary first** — everything else (manifest loader,
bundle unpacker, real algorithms, the four canonical gates) builds on the bridge
existing and being uncrossable.

## Prerequisites

- Rust stable (workspace already has `wasm32-wasip2` from `SETUP.md`; not needed
  for the host itself).
- Deno: `brew install deno`. The host shells out to whatever `deno` is on PATH.
  The sidecar is pinned to `pyodide@0.26.2` via npm.

## Smoke test

From `commonsc/crates/host/`:

```sh
cargo run -- hello "2 + 3"
```

What happens:

1. Rust spawns `deno run --no-prompt --allow-read --allow-env sidecar/run.ts`.
2. The sidecar loads Pyodide (~10s on first run while npm fetches it; cached
   afterwards) and emits `{"type":"ready"}` on stdout.
3. Rust writes `{"type":"hello","expr":"2 + 3"}` to the sidecar's stdin.
4. The sidecar evaluates the expression with `pyodide.runPython(...)` and
   replies `{"type":"result","value":5}`.
5. Rust prints `5` and shuts the sidecar down.

If the first run hangs at "loading Pyodide", it's the npm fetch — `deno cache
sidecar/run.ts` from inside `sidecar/` warms the cache up front.

## What this does NOT do yet

- Read the manifest. Hello mode hardcodes `expr` and bypasses the schema entirely.
- Verify any signatures. Bundles aren't even mounted; nothing crosses the boundary
  except the Python expression literal.
- Stream a `VariantSet`. That's the next milestone — the bridge protocol has
  placeholders for `progress` and structured `result` events, but the only command
  wired so far is `hello`.
- Prove the privacy boundary by negative test. The sidecar is launched without
  `--allow-net` etc., but we don't yet run "naughty" fixtures that *try* to escape
  and confirm they fail. That's task #9.

## Bridge protocol

Parent ➝ child commands:

| `type`     | Fields              | Meaning                                            |
|------------|---------------------|----------------------------------------------------|
| `hello`    | `expr: string`      | Eval Python expression, return result.            |
| `shutdown` | —                   | Exit cleanly.                                     |

Child ➝ parent events:

| `type`     | Fields                                  | Meaning                          |
|------------|-----------------------------------------|----------------------------------|
| `ready`    | —                                       | Pyodide booted.                  |
| `result`   | `value: unknown`                        | Final return value.              |
| `progress` | `percent: number`, `label?: string`     | Typed progress (rate-limited).   |
| `log`      | `level: "debug"\|"info"\|"warn"`, `message: string` | Diagnostic; never user data. |
| `error`    | `message: string`                       | Failure; algorithm or runtime.   |

All on stdin/stdout, one JSON object per line. The child's stderr is passed
through to the host's stderr; use it for unstructured debug output during
development.

## File layout

```
commonsc-host/
├── Cargo.toml
├── src/
│   ├── main.rs         # CLI: `commonsc-host hello "<expr>"`
│   ├── lib.rs
│   └── sidecar.rs      # spawn + JSON-line bridge
├── sidecar/
│   ├── run.ts          # Deno entry, loads Pyodide
│   └── deno.json       # pins pyodide@0.26.2
└── fixtures/           # algorithm fixtures used by later milestones
```
