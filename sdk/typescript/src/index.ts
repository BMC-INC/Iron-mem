/**
 * IronMem TypeScript SDK — a thin typed client over the IronMem REST API.
 *
 * ```ts
 * import { IronMem } from "ironmem";
 * const mem = new IronMem("http://127.0.0.1:37778", { token: "<agent key>" });
 * await mem.remember({ project: "/my/project", text: "Alice prefers dark roast", kind: "preference" });
 * const hits = await mem.context({ project: "/my/project", query: "what coffee does Alice like?" });
 * const lineage = await mem.lineage(hits.memories[0].id);
 * const report = await mem.complianceReport();
 * ```
 *
 * Zero dependencies (global `fetch`, Node 18+ / browsers / Deno / Bun). The
 * server enforces all governance — namespace allowlists per agent key,
 * consent gates, the hash-chained ledger; the SDK is transport only.
 */

export interface IronMemOptions {
  token?: string;
  timeoutMs?: number;
}

export interface RememberParams {
  project: string;
  text: string;
  scope?: "project" | "user";
  kind?: string;
  tags?: string;
  namespace?: string;
  source_type?: string;
  trust_tier?: "high" | "medium" | "low" | "untrusted";
  writer_identity?: string;
  classification?: "public" | "internal" | "confidential" | "restricted" | "phi" | "pii";
  consent_state?: "required" | "granted" | "denied" | "withdrawn";
  residency?: string;
  retention_policy_id?: string;
  expires_at?: number;
  legal_hold?: boolean;
  source_ref?: string;
}

export interface ContextParams {
  project: string;
  query?: string;
  limit?: number;
  namespace?: string;
  rerank?: boolean;
  pool?: number;
}

export interface LedgerEntry {
  id: number;
  namespace: string;
  memory_id: number | null;
  op_type: string;
  actor: string | null;
  prev_hash: string | null;
  entry_hash: string;
  payload: string;
  created_at: number;
}

export interface InjectionEvent {
  project: string;
  session_id: string | null;
  rank: number;
  query: string | null;
  created_at: number;
}

export interface MemoryLineage {
  memory_id: number;
  summary: string | null;
  project: string | null;
  namespace: string | null;
  kind: string | null;
  writer_identity: string | null;
  source_type: string | null;
  trust_tier: string | null;
  classification: string | null;
  consent_state: string | null;
  retention_policy_id: string | null;
  legal_hold: boolean;
  tombstoned_at: number | null;
  parent_chain: number[];
  ledger: LedgerEntry[];
  injections: InjectionEvent[];
}

export interface ChainVerification {
  namespace: string;
  entries: number;
  valid: boolean;
  first_broken_id: number | null;
}

export interface ComplianceReport {
  generated_at: string;
  chains: ChainVerification[];
  inventory: Array<{
    namespace: string;
    classification: string;
    consent_state: string | null;
    total: number;
    legal_holds: number;
    tombstoned: number;
    with_expiry: number;
    with_retention_policy: number;
  }>;
  snapshots: Array<{
    id: string;
    label: string | null;
    project: string | null;
    memory_count: number;
    edge_count: number;
    created_at: number;
  }>;
}

export class IronMemError extends Error {
  constructor(
    public status: number,
    public body: string,
  ) {
    super(`IronMem API error ${status}: ${body}`);
    this.name = "IronMemError";
  }
}

export class IronMem {
  private baseUrl: string;
  private token?: string;
  private timeoutMs: number;

  constructor(baseUrl = "http://127.0.0.1:37778", opts: IronMemOptions = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.token = opts.token;
    this.timeoutMs = opts.timeoutMs ?? 30_000;
  }

  private async request<T>(
    method: string,
    path: string,
    params?: Record<string, unknown>,
    body?: Record<string, unknown>,
  ): Promise<T> {
    let url = this.baseUrl + path;
    if (params) {
      const qs = new URLSearchParams();
      for (const [k, v] of Object.entries(params)) {
        if (v !== undefined && v !== null) qs.set(k, String(v));
      }
      const encoded = qs.toString();
      if (encoded) url += `?${encoded}`;
    }
    const headers: Record<string, string> = { Accept: "application/json" };
    if (this.token) headers.Authorization = `Bearer ${this.token}`;
    let payload: string | undefined;
    if (body) {
      headers["Content-Type"] = "application/json";
      payload = JSON.stringify(
        Object.fromEntries(Object.entries(body).filter(([, v]) => v !== undefined && v !== null)),
      );
    }
    const resp = await fetch(url, {
      method,
      headers,
      body: payload,
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    if (!resp.ok) throw new IronMemError(resp.status, await resp.text());
    const text = await resp.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  // ── session lifecycle ─────────────────────────────────────────────────────

  sessionStart(project: string): Promise<{ session_id: string }> {
    return this.request("POST", "/session/start", undefined, { project });
  }

  recordEvent(sessionId: string, project: string, tool: string, input?: string, output?: string): Promise<unknown> {
    return this.request("POST", "/event", undefined, { session_id: sessionId, project, tool, input, output });
  }

  sessionEnd(sessionId: string): Promise<unknown> {
    return this.request("POST", "/session/end", undefined, { session_id: sessionId });
  }

  compress(sessionId: string): Promise<unknown> {
    return this.request("POST", "/compress", undefined, { session_id: sessionId });
  }

  // ── memories ──────────────────────────────────────────────────────────────

  /** Store an explicit governed memory. PII/PHI requires consent_state="granted" — the server fails closed. */
  remember(params: RememberParams): Promise<{ memory_id: number; namespace: string }> {
    return this.request("POST", "/remember", undefined, { ...params });
  }

  /** Ranked memory context: hybrid retrieval when `query` is set, recent memories otherwise. */
  context(params: ContextParams): Promise<any> {
    const { rerank, ...rest } = params;
    return this.request("GET", "/context", { ...rest, rerank: rerank === undefined ? undefined : rerank ? "1" : "0" });
  }

  skim(project: string, limit?: number): Promise<any> {
    return this.request("GET", "/skim", { project, limit });
  }

  feedback(memoryId: number, signal: string, project: string, opts: { weight?: number; detail?: string } = {}): Promise<unknown> {
    return this.request("POST", "/feedback", undefined, {
      memory_id: memoryId,
      signal,
      project,
      weight: opts.weight ?? 1.0,
      detail: opts.detail,
    });
  }

  // ── governance & compliance ───────────────────────────────────────────────

  /** Memory→action lineage: writer, governance, ledger trail, every injection into an agent context. */
  lineage(memoryId: number): Promise<MemoryLineage> {
    return this.request("GET", `/memory/${memoryId}/lineage`);
  }

  /** EU AI Act Art. 12/13 report: hash-chain verification, governance inventory, snapshots. */
  complianceReport(): Promise<ComplianceReport> {
    return this.request("GET", "/compliance/report");
  }

  snapshots(limit?: number): Promise<any> {
    return this.request("GET", "/snapshots", { limit });
  }

  status(): Promise<any> {
    return this.request("GET", "/status");
  }

  profile(): Promise<any> {
    return this.request("GET", "/profile");
  }
}

export default IronMem;
