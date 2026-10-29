#!/usr/bin/env python3
"""Exercise authenticated Streamable HTTP using the official MCP SDK and a disposable database.

Install test-only clients with: pip install mcp==1.30.0
"""
import argparse
import asyncio
import importlib.metadata
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import uuid

import httpx
from mcp import ClientSession
from mcp.client.streamable_http import streamable_http_client


async def smoke(binary, expected_version):
    with tempfile.TemporaryDirectory(prefix="ironmem-http-sdk-") as directory:
        root = Path(directory)
        project = root / "project"
        project.mkdir()
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        token = uuid.uuid4().hex
        config = root / "settings.json"
        config.write_text(json.dumps({
            "db_path": str(root / "memory.db"), "auth_token": token,
            "mcp_sse_port": port, "embedding": {"provider": "none"},
            "compression": {"mode": "local"}, "rerank": {"enabled": False},
            "observer": {"enabled": False}, "auto_dream": {"enabled": False},
        }))
        env = {key: value for key, value in os.environ.items() if key in ("PATH", "TMPDIR", "TEMP", "TMP", "SystemRoot")}
        with (root / "server.log").open("w") as log:
            server = subprocess.Popen([str(binary.resolve()), "--config", str(config), "serve"],
                cwd=project, env=env, stdout=log, stderr=log)
            url = f"http://127.0.0.1:{port}/mcp"
            try:
                async with httpx.AsyncClient(timeout=5) as probe:
                    for _ in range(100):
                        if server.poll() is not None:
                            raise RuntimeError("Temporary MCP server exited before readiness")
                        try:
                            response = await probe.get(url)
                            assert response.status_code == 401
                            break
                        except httpx.ConnectError:
                            await asyncio.sleep(0.1)
                    else:
                        raise TimeoutError("Temporary MCP server did not start")
                    malicious = await probe.post(url, headers={"Authorization": f"Bearer {token}",
                        "Origin": "https://untrusted.example"}, json={})
                    assert malicious.status_code == 403
                async with httpx.AsyncClient(headers={"Authorization": f"Bearer {token}"}, timeout=20) as http:
                    async with streamable_http_client(url, http_client=http) as (read, write, session_id):
                        async with ClientSession(read, write) as client:
                            initialized = await client.initialize()
                            assert initialized.serverInfo.version == expected_version
                            tools = (await client.list_tools()).tools
                            baseline = json.loads((Path(__file__).resolve().parent.parent / "tests/fixtures/mcp_tools_before_rmcp_3.json").read_text())
                            assert [t.model_dump(by_alias=True, exclude_none=True) for t in tools] == baseline
                            marker = "HTTP SDK canary amber harbor preserves project memory."
                            saved = await client.call_tool("remember", {"project": str(project),
                                "text": marker, "scope": "project", "kind": "project_config", "classification": "public"})
                            assert not saved.isError
                            found = await client.call_tool("search_memories", {"project": str(project),
                                "query": "amber", "semantic": False, "limit": 3})
                            assert not found.isError and marker in found.model_dump_json()
                            assert session_id() is None
                            return {"ok": True, "client": "official MCP Python SDK",
                                "sdk_version": importlib.metadata.version("mcp"), "transport": "authenticated Streamable HTTP",
                                "protocol": initialized.protocolVersion, "server": initialized.serverInfo.model_dump(),
                                "tools": len(tools), "prior_tool_contract_preserved": True, "save_and_retrieve": True,
                                "unauthenticated_rejected": True, "untrusted_origin_rejected": True,
                                "stateless": True, "personal_database_opened": False}
            finally:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    print(json.dumps(asyncio.run(asyncio.wait_for(smoke(args.binary, args.expected_version), 90)), indent=2))
