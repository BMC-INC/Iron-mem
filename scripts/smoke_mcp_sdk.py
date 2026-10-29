#!/usr/bin/env python3
"""Test the release binary with the official MCP Python client and disposable data.

Install the test-only dependency in a virtual environment: pip install mcp==1.30.0
"""
import argparse
import asyncio
import importlib.metadata
import json
from pathlib import Path
import tempfile
import subprocess

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


async def smoke(binary, expected_version):
    with tempfile.TemporaryDirectory(prefix="ironmem-sdk-") as directory:
        root = Path(directory)
        project = root / "project"
        project.mkdir()
        config = root / "settings.json"
        config.write_text(json.dumps({
            "db_path": str(root / "memory.db"),
            "embedding": {"provider": "none"}, "compression": {"mode": "local"},
            "rerank": {"enabled": False}, "observer": {"enabled": False},
            "auto_dream": {"enabled": False},
        }))
        parameters = StdioServerParameters(command=str(binary.resolve()),
            args=["--config", str(config), "mcp"], cwd=str(project), env={})
        async with stdio_client(parameters) as (read, write):
            async with ClientSession(read, write) as client:
                initialized = await client.initialize()
                version = subprocess.check_output([str(binary.resolve()), "--version"], text=True).split()[1]
                assert initialized.serverInfo.version == version == expected_version, "MCP/CLI/release versions disagree"
                names = {tool.name for tool in (await client.list_tools()).tools}
                assert {"get_status", "remember", "search_memories"} <= names
                status = await client.call_tool("get_status", {})
                assert not status.isError
                assert json.loads(status.content[0].text)["memories"] == 0
                marker = "MCP SDK release canary cobalt-harbor-791 uses local project memory."
                saved = await client.call_tool("remember", {"project": str(project),
                    "text": marker, "scope": "project", "kind": "project_config",
                    "classification": "public"})
                assert not saved.isError
                found = await client.call_tool("search_memories", {"project": str(project),
                    "query": "cobalt-harbor-791", "semantic": False, "limit": 3})
                assert not found.isError and marker in found.model_dump_json()
                return {"ok": True, "client": "official MCP Python SDK",
                    "sdk_version": importlib.metadata.version("mcp"),
                    "protocol": initialized.protocolVersion,
                    "server": initialized.serverInfo.model_dump(), "tools": len(names),
                    "save_and_retrieve": True, "personal_database_opened": False}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    print(json.dumps(asyncio.run(asyncio.wait_for(smoke(args.binary, args.expected_version), 60)), indent=2))
