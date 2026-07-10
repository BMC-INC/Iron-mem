# IronMem Memory Graph Workbench Design

## Product Direction

The Memory Graph Workbench turns IronMem's existing temporal graph, typed memories, CCR chunks, governance metadata, and retrieval pipeline into one operational surface. Its purpose is investigation rather than decoration: every visible relationship must lead back to the memory that produced it, the structured chunks extracted from it, and the original transcript when available. The graph is therefore the primary workspace, with evidence and retrieval reasoning arranged around it instead of hidden behind separate pages.

The visual direction is industrial and editorial: near-black iron, warm paper-white text, oxidized copper for active relationships, electric cyan for selected paths, and restrained red for conflicts or superseded history. A dense three-column layout keeps filters on the left, an unframed canvas in the center, and the selected node or edge evidence inspector on the right. The interface must remain legible at laptop widths and collapse into a graph-first stacked workspace on mobile.

## Architecture

Two additive HTTP APIs support the workbench. `GET /api/graph/window` returns a bounded edge window filtered by project, free-text graph query, valid date, and history visibility. It deliberately caps result size so the browser never attempts to lay out the full local database. `GET /api/memories/{id}/evidence` returns one memory, its safe governance metadata, chunks, and graph edges. Verbatim source remains behind the existing `POST /retrieve_original` expansion path and is fetched only after an explicit user action.

The browser remains dependency-free and is served from the Rust binary. A canvas force layout renders nodes and directed edges without network-loaded scripts. DOM controls provide accessible search, filters, timeline controls, list views, status, and the evidence inspector. Graph queries can optionally run `/context` for a selected project, allowing the workbench to display the exact evidence chain IronMem would supply to an agent and highlight those memories in the graph.

## Interaction And Failure Model

The default view shows recent active relationships across projects. Search, project, relation state, valid date, and edge-count controls update the graph without a page reload. Selecting a node focuses its neighborhood; selecting an edge opens provenance. Retrieval Trace runs a real context query, lists ranked evidence chains, and marks graph edges backed by the returned memories. Users can open the exact original source, copy a chunk identifier, fit the graph, or reset focus.

Every request has loading, empty, and error states. Stale requests are ignored through request sequencing. Canvas resizing preserves the current graph rather than resetting it. Keyboard users can operate all DOM controls and inspect the same graph data through the relationship list. Destructive memory and edge operations remain outside the primary investigation flow and keep their existing governed endpoints.

## Verification

Database tests cover bounded ordering, project filtering, normalized free-text matching, history visibility, and valid-time filtering. API smoke tests exercise the new routes against an isolated SQLite database. The full Rust suite, MCP stdio test, clippy with `local-onnx`, and release build must pass. Browser verification covers desktop and mobile screenshots, nonblank canvas pixels, graph selection, evidence expansion, retrieval trace, network responses, console cleanliness, responsive overflow, and accessible names.
