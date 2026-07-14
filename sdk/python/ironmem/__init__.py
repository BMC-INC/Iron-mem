"""IronMem Python SDK — a thin typed client over the IronMem REST API.

Usage:
    from ironmem import IronMem

    mem = IronMem("http://127.0.0.1:37778", token="<agent key or auth token>")
    mem.remember("/my/project", "Alice prefers dark roast", kind="preference")
    hits = mem.context("/my/project", query="what coffee does Alice like?")
    lineage = mem.lineage(hits[0]["id"])
    report = mem.compliance_report()

Only the Python standard library is used (urllib), so the SDK has zero
dependencies. Every method mirrors one REST route; the server enforces
governance (namespaces, consent, agent-key allowlists) — the SDK adds nothing
security-relevant on the client side.
"""

from __future__ import annotations

import json
import urllib.parse
import urllib.request
from typing import Any, Optional

__all__ = ["IronMem", "IronMemError"]
__version__ = "0.1.0"


class IronMemError(RuntimeError):
    """Raised for any non-2xx response, carrying status and server message."""

    def __init__(self, status: int, message: str):
        super().__init__(f"IronMem API error {status}: {message}")
        self.status = status
        self.message = message


class IronMem:
    def __init__(
        self,
        base_url: str = "http://127.0.0.1:37778",
        token: Optional[str] = None,
        timeout: float = 30.0,
    ):
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = timeout

    # ── transport ───────────────────────────────────────────────────────────

    def _request(
        self,
        method: str,
        path: str,
        params: Optional[dict[str, Any]] = None,
        body: Optional[dict[str, Any]] = None,
    ) -> Any:
        url = self.base_url + path
        if params:
            filtered = {k: v for k, v in params.items() if v is not None}
            if filtered:
                url += "?" + urllib.parse.urlencode(filtered)
        data = None
        headers = {"Accept": "application/json"}
        if body is not None:
            data = json.dumps({k: v for k, v in body.items() if v is not None}).encode()
            headers["Content-Type"] = "application/json"
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as e:
            raise IronMemError(e.code, e.read().decode(errors="replace")) from None
        if not raw:
            return None
        return json.loads(raw)

    # ── session lifecycle ───────────────────────────────────────────────────

    def session_start(self, project: str) -> dict:
        return self._request("POST", "/session/start", body={"project": project})

    def record_event(
        self,
        session_id: str,
        project: str,
        tool: str,
        input: Optional[str] = None,
        output: Optional[str] = None,
    ) -> dict:
        return self._request(
            "POST",
            "/event",
            body={
                "session_id": session_id,
                "project": project,
                "tool": tool,
                "input": input,
                "output": output,
            },
        )

    def session_end(self, session_id: str) -> dict:
        return self._request("POST", "/session/end", body={"session_id": session_id})

    def compress(self, session_id: str) -> dict:
        return self._request("POST", "/compress", body={"session_id": session_id})

    # ── memories ────────────────────────────────────────────────────────────

    def remember(
        self,
        project: str,
        text: str,
        *,
        scope: Optional[str] = None,
        kind: Optional[str] = None,
        tags: Optional[str] = None,
        namespace: Optional[str] = None,
        source_type: Optional[str] = None,
        trust_tier: Optional[str] = None,
        writer_identity: Optional[str] = None,
        classification: Optional[str] = None,
        consent_state: Optional[str] = None,
        residency: Optional[str] = None,
        retention_policy_id: Optional[str] = None,
        expires_at: Optional[int] = None,
        legal_hold: Optional[bool] = None,
        source_ref: Optional[str] = None,
    ) -> dict:
        """Store an explicit governed memory. PII/PHI classifications require
        consent_state='granted' — the server fails closed otherwise."""
        return self._request(
            "POST",
            "/remember",
            body={
                "project": project,
                "text": text,
                "scope": scope,
                "kind": kind,
                "tags": tags,
                "namespace": namespace,
                "source_type": source_type,
                "trust_tier": trust_tier,
                "writer_identity": writer_identity,
                "classification": classification,
                "consent_state": consent_state,
                "residency": residency,
                "retention_policy_id": retention_policy_id,
                "expires_at": expires_at,
                "legal_hold": legal_hold,
                "source_ref": source_ref,
            },
        )

    def context(
        self,
        project: str,
        *,
        query: Optional[str] = None,
        limit: Optional[int] = None,
        namespace: Optional[str] = None,
        rerank: Optional[bool] = None,
        pool: Optional[int] = None,
    ) -> Any:
        """Ranked memory context: hybrid retrieval when `query` is given,
        recent memories otherwise."""
        return self._request(
            "GET",
            "/context",
            params={
                "project": project,
                "query": query,
                "limit": limit,
                "namespace": namespace,
                "rerank": ("1" if rerank else None) if rerank is not None else None,
                "pool": pool,
            },
        )

    def skim(self, project: str, limit: Optional[int] = None) -> Any:
        return self._request("GET", "/skim", params={"project": project, "limit": limit})

    def feedback(
        self,
        memory_id: int,
        signal: str,
        project: str,
        *,
        weight: float = 1.0,
        detail: Optional[str] = None,
    ) -> Any:
        return self._request(
            "POST",
            "/feedback",
            body={
                "memory_id": memory_id,
                "signal": signal,
                "project": project,
                "weight": weight,
                "detail": detail,
            },
        )

    # ── governance & compliance ─────────────────────────────────────────────

    def lineage(self, memory_id: int) -> dict:
        """Memory→action lineage: writer, governance, ledger trail, and every
        injection of this memory into an agent context."""
        return self._request("GET", f"/memory/{memory_id}/lineage")

    def compliance_report(self) -> dict:
        """EU AI Act Art. 12/13 report: hash-chain verification per namespace,
        governance inventory, snapshot versions."""
        return self._request("GET", "/compliance/report")

    def snapshots(self, limit: Optional[int] = None) -> Any:
        return self._request("GET", "/snapshots", params={"limit": limit})

    def status(self) -> dict:
        return self._request("GET", "/status")

    def profile(self) -> Any:
        return self._request("GET", "/profile")
