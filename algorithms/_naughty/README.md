# Naughty fixtures

Algorithms whose explicit purpose is to attempt something a Tier-1 plugin must
not be able to do — open a socket, read outside the bundle, spawn a
subprocess, fetch a URL. Each fixture wraps the bad action in a try/except and
returns a `Result` envelope reporting which outcome occurred:

| Outcome | tone |
|---|---|
| Boundary held — the action raised an exception | `moss` |
| Boundary breach — the action succeeded | `rust` |

These bundles are run by `commonsc-host/tests/privacy.rs`, which builds them
in-process and asserts every fixture comes back with `tone: moss`. They never
ship to the customer registry — devkit publish writes them to a temp
directory only.

The privacy invariant from the brief §3.2 is "absence of capability, not
trust." These tests prove the absence: each bad action must be unreachable by
construction (Pyodide WASM has no native sockets, the virtual FS contains only
what we mount, subprocess module is not available, Deno is spawned without
`--allow-net`).
