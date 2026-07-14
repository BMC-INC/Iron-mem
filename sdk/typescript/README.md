# IronMem TypeScript SDK

Zero-dependency TypeScript client for the [IronMem](https://github.com/BMC-INC/Iron-mem) REST API — governed persistent memory for AI agents. Node 18+, browsers, Deno, Bun (uses global `fetch`).

```bash
npm install ./sdk/typescript      # from a checkout (npm publishing TBD)
```

```ts
import { IronMem } from "ironmem";

const mem = new IronMem("http://127.0.0.1:37778", { token: "<agent key>" });

// store a governed memory (PII fails closed without granted consent)
await mem.remember({ project: "/my/project", text: "Alice prefers dark roast", kind: "preference" });

// ranked recall
const hits = await mem.context({ project: "/my/project", query: "what coffee does Alice like?", limit: 5 });

// governance: who wrote it, and every agent context it influenced
const lineage = await mem.lineage(hits.memories[0].id);

// EU AI Act Art. 12/13 evidence, hash-chain verified
const report = await mem.complianceReport();
console.assert(report.chains.every((c) => c.valid));
```

The server enforces all governance (namespace allowlists per agent key, consent
gates, ledger); the SDK is a thin transport. Errors surface as `IronMemError`
with the HTTP status and server message. Compliance/lineage responses are fully
typed (`MemoryLineage`, `ComplianceReport`, `ChainVerification`).

Start the server with `ironmem serve` (or point at your deployment).
