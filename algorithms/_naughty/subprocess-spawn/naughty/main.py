"""Tries to spawn a subprocess via os.system and subprocess.run.

Pyodide WASM has no fork/exec — `os.system` is stubbed to return 0 without
actually spawning anything, which is technically privacy-preserving (no real
process runs, no data leaves) but easy to misread as a breach.

This fixture instead probes for *observable* side effects: subprocess.run with
captured stdout, then check whether any bytes were produced. Empty captured
output means the call was a no-op stub — the boundary held.
"""
from __future__ import annotations

import time
from typing import Any

ID = "_naughty/subprocess-spawn"


def _report(summary: str, tone: str, detail: str | None = None, kind: str | None = None) -> dict[str, Any]:
    block_rows = [
        {"label": "Outcome", "value": summary, "tone": tone},
        {"label": "Error",   "value": detail or "—"},
        {"label": "Type",    "value": kind or "—"},
    ]
    out: dict[str, Any] = {
        "schemaVersion": "1",
        "algorithmId": ID,
        "algorithmVersion": "0.0.1",
        "computedAt": int(time.time() * 1000),
        "summary": summary,
        "tone": tone,
        "blocks": [
            {"kind": "rows", "title": "Observation", "rows": block_rows},
        ],
    }
    if detail:
        out["detail"] = detail
    return out


def compute(variant_set: dict[str, Any]) -> dict[str, Any]:
    breaches: list[str] = []
    holds: list[str] = []

    # os.system in Pyodide is a no-op stub returning 0. We record what it
    # reports but don't treat any rc by itself as a breach — only observable
    # side effects matter.
    try:
        import os
        rc = os.system("ls /")
        holds.append(f"os.system → rc={rc} (no observable child output)")
    except Exception as e:
        holds.append(f"os.system → {type(e).__name__}: {e}")

    # subprocess.run with capture_output gives us a stdout buffer we can
    # inspect. If a real `ls /` ran we'd see directory entries; an empty buffer
    # means the call didn't actually exec anything.
    try:
        import subprocess
        out = subprocess.run(["ls", "/"], capture_output=True, check=False, timeout=2)
        if len(out.stdout) > 0 or len(out.stderr) > 0:
            breaches.append(
                f"subprocess.run produced output: stdout={out.stdout[:64]!r} "
                f"stderr={out.stderr[:64]!r}"
            )
        else:
            holds.append(f"subprocess.run rc={out.returncode}, stdout+stderr empty")
    except Exception as e:
        holds.append(f"subprocess.run → {type(e).__name__}: {e}")

    if breaches:
        return _report(
            "BOUNDARY BREACH: subprocess produced output",
            tone="rust",
            detail="; ".join(breaches),
        )
    return _report(
        "Boundary held: subprocess spawn produced no observable side effects",
        tone="moss",
        detail="; ".join(holds),
    )
