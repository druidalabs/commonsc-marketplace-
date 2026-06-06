"""Tries to read /etc/passwd. Pyodide's virtual FS contains only what the host
mounts under /algorithm/<uuid>; an absolute path outside that must fail."""
from __future__ import annotations

import time
from typing import Any

ID = "_naughty/fs-escape"


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
    targets = ["/etc/passwd", "/etc/hosts", "/Users"]
    for target in targets:
        try:
            with open(target, "rb") as f:
                first = f.read(64)
            return _report(
                f"BOUNDARY BREACH: read {target}",
                tone="rust",
                detail=f"first 64 bytes: {first!r}",
            )
        except Exception as e:
            # Try the next target — only return "held" once everything is blocked.
            last_err = (str(e), type(e).__name__)
    return _report(
        "Boundary held: host filesystem unreachable",
        tone="moss",
        detail=last_err[0],
        kind=last_err[1],
    )
