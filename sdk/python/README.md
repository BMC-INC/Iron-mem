# IronMem Python SDK

Zero-dependency Python client for the [IronMem](https://github.com/BMC-INC/Iron-mem) REST API — governed persistent memory for AI agents.

```bash
pip install ./sdk/python        # from a checkout (PyPI publishing TBD)
```

```python
from ironmem import IronMem

mem = IronMem("http://127.0.0.1:37778", token="<agent key>")

# store a governed memory (PII fails closed without granted consent)
mem.remember("/my/project", "Alice prefers dark roast", kind="preference")

# ranked recall
hits = mem.context("/my/project", query="what coffee does Alice like?", limit=5)

# governance: who wrote it, and every agent context it influenced
lineage = mem.lineage(hits[0]["id"])

# EU AI Act Art. 12/13 evidence, hash-chain verified
report = mem.compliance_report()
assert all(c["valid"] for c in report["chains"])
```

The server enforces all governance (namespace allowlists per agent key, consent
gates, ledger); the SDK is a thin transport. Errors surface as `IronMemError`
with the HTTP status and server message.

Start the server with `ironmem serve` (or point at your deployment). See the
repo README for the full route list; every SDK method maps to one route.
