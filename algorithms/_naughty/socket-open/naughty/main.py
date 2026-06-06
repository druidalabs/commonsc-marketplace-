"""Tries to open a TCP socket. The sandbox boundary must prevent any reachable
network endpoint, so this is expected to raise."""
from __future__ import annotations

import time
from typing import Any

ID = "_naughty/socket-open"


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
        import socket
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.settimeout(1.0)
        sock.connect(("127.0.0.1", 80))
        sock.close()
        return _report(
            "BOUNDARY BREACH: opened TCP socket to 127.0.0.1:80",
            tone="rust",
            detail="connect() returned without raising — network was reachable",
        )
    except Exception as e:
        return _report(
            "Boundary held: socket open rejected",
            tone="moss",
            detail=str(e),
            kind=type(e).__name__,
        )
