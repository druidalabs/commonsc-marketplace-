"""Tries to fetch an HTTPS URL. Pyodide's urllib uses the host's network
adapter (XHR/fetch in browser; Deno fetch in our setup) — and Deno is spawned
without --allow-net, so the request must fail."""
from __future__ import annotations

import time
from typing import Any

ID = "_naughty/network-fetch"


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
    try:
        from urllib.request import urlopen
        resp = urlopen("https://example.com", timeout=2)
        body = resp.read(128)
        return _report(
            "BOUNDARY BREACH: urlopen returned",
            tone="rust",
            detail=f"got {len(body)} bytes from https://example.com",
        )
    except Exception as e:
        return _report(
            "Boundary held: HTTP fetch rejected",
            tone="moss",
            detail=str(e),
            kind=type(e).__name__,
        )
