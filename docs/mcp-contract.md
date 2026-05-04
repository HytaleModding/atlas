# Atlas MCP Contract

**Contract version:** `1.0.0`
**Audience:** agents (Claude Code, Cursor, internal tooling) that talk to
Atlas over MCP. This document is the authoritative schema; the local
(desktop) and hosted (`atlas-serve`, future) implementations are
interchangeable as far as any client is concerned.

## Transport

- Local: `http://127.0.0.1:<port>/mcp` - served from the same Axum
  router that hosts `/healthz` (`src-tauri/src/http.rs`). Port is
  logged at startup.
- Hosted (future): `https://<atlas-serve-host>/mcp`. Same router.

JSON-RPC 2.0 framed per MCP spec. Every tool call passes its input
through MCP's `CallTool` request; responses come back through
`CallTool` result with structured content.

## Versioning

Three version dimensions, kept distinct:

| Dimension        | Where it lives                        | Changes when                                      |
|------------------|---------------------------------------|---------------------------------------------------|
| Contract version | this document (`1.0.0`)               | Tool shape or error taxonomy changes. Semver.     |
| Artifact schema  | `manifest.schema_version` (integer)   | Tarball layout or index format changes.           |
| Embedder id      | `manifest.embedder_id` (string)       | BGE model swap / quantization change.             |

A client pins to a contract version (major). Servers must accept any
minor/patch within that major. Breaking changes mean a new major; the
server advertises its supported majors in the `initialize` handshake.

## Build-id addressing

Every tool accepts an optional `build_id` parameter. When omitted, the
server routes to the catalog's "active" build (desktop: user's
selection; `atlas-serve`: most-recent release-channel artifact). When
present, the server uses that exact build or returns
`ArtifactVersionMismatch` if it is not mounted.

Build ids are stable and opaque to clients - format is
`<patchline>-<impl_version_short_sha>`, e.g.
`release-2026.03.26-89796e57b`. Treat them as strings.

## Source-type discriminator

Phase 3 ships with `source_type = "source"` only. Phase 4 adds
`"hm_doc"`, `"hypixel_doc"`, `"asset"`. Clients should treat unknown
source types as opaque and pass them through untouched. Tools that
take a `source_type` filter accept either a single value or a list.

---

## Tools

### 1. `search`

Hybrid keyword + semantic search over one mounted artifact.

**Input:**
```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "required": ["query"],
  "additionalProperties": false,
  "properties": {
    "query": {
      "type": "string",
      "minLength": 1,
      "maxLength": 1024,
      "description": "Natural-language or keyword query. Tokenized by Tantivy on the BM25 side and embedded with the artifact's embedder_id on the vector side."
    },
    "limit": {
      "type": "integer",
      "minimum": 1,
      "maximum": 100,
      "default": 25
    },
    "source_type": {
      "description": "Restrict results to one or more source types. Omit for all.",
      "oneOf": [
        { "type": "string", "enum": ["source", "hm_doc", "hypixel_doc", "asset"] },
        { "type": "array", "items": { "type": "string", "enum": ["source", "hm_doc", "hypixel_doc", "asset"] }, "minItems": 1 }
      ]
    },
    "build_id": {
      "type": "string",
      "description": "Override the active build. Must be a currently-mounted build id."
    }
  }
}
```

**Output:**
```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "required": ["hits", "query", "build_id", "elapsed_ms"],
  "additionalProperties": false,
  "properties": {
    "build_id": { "type": "string" },
    "query":    { "type": "string" },
    "elapsed_ms": { "type": "integer", "minimum": 0 },
    "partial": {
      "type": "boolean",
      "default": false,
      "description": "True when vector search was unavailable and results are keyword-only. See ArtifactPartiallyAvailable guidance."
    },
    "hits": {
      "type": "array",
      "items": { "$ref": "#/definitions/Hit" }
    }
  },
  "definitions": {
    "Hit": {
      "type": "object",
      "required": ["path", "fqn", "score", "chunk_kind", "source_type"],
      "properties": {
        "source_type": { "type": "string" },
        "path":     { "type": "string", "description": "Artifact-relative path to the file containing the hit." },
        "fqn":      { "type": "string", "description": "Fully-qualified name (class/method). May equal `path` for docs/assets." },
        "package":  { "type": "string" },
        "filename": { "type": "string" },
        "score":    { "type": "number", "description": "RRF score; higher = better. Not comparable across queries." },
        "line_count": { "type": "integer", "minimum": 0 },
        "start_line": { "type": ["integer", "null"], "minimum": 1 },
        "end_line":   { "type": ["integer", "null"], "minimum": 1 },
        "preview_line": { "type": ["integer", "null"], "minimum": 1 },
        "preview":  { "type": ["string", "null"], "description": "Plain-text excerpt around the best-scoring chunk. Never HTML." },
        "chunk_kind": { "type": "string", "enum": ["type", "method", "file", "doc", "asset"] },
        "symbol_name": { "type": "string" }
      }
    }
  }
}
```

**Errors:** `IndexNotMounted`, `ArtifactVersionMismatch`,
`InvalidQuery`, `RateLimited`.

---

### 2. `get_source`

Read the full text of a source file from an artifact's decompile tree.
Intended for follow-up after `search` returns a `source` hit.

**Input:**
```json
{
  "type": "object",
  "required": ["path"],
  "additionalProperties": false,
  "properties": {
    "path":     { "type": "string", "description": "Artifact-relative path as returned by `search`." },
    "build_id": { "type": "string" },
    "start_line": { "type": "integer", "minimum": 1 },
    "end_line":   { "type": "integer", "minimum": 1 }
  }
}
```

`start_line`/`end_line`, if provided, clip the returned text to that
inclusive range. Both must be set or neither. `end_line >= start_line`.

**Output:**
```json
{
  "type": "object",
  "required": ["path", "build_id", "content", "line_count"],
  "properties": {
    "path":      { "type": "string" },
    "build_id":  { "type": "string" },
    "content":   { "type": "string" },
    "line_count": { "type": "integer", "minimum": 0 },
    "truncated": { "type": "boolean", "default": false }
  }
}
```

**Errors:** `IndexNotMounted`, `SourceNotFound`, `ArtifactVersionMismatch`.

---

### 3. `get_doc`

Read a documentation page from the artifact. HM docs + Hypixel docs
share this tool, dispatched by `source_type`.

**Status:** tool surface live in Phase 3. Returns `SourceTypeNotIndexed`
until Phase 4 ingestion lands.

**Input:**
```json
{
  "type": "object",
  "required": ["path", "source_type"],
  "properties": {
    "path":        { "type": "string" },
    "source_type": { "type": "string", "enum": ["hm_doc", "hypixel_doc"] },
    "build_id":    { "type": "string" }
  }
}
```

**Output:**
```json
{
  "type": "object",
  "required": ["path", "source_type", "build_id", "content"],
  "properties": {
    "path":        { "type": "string" },
    "source_type": { "type": "string" },
    "build_id":    { "type": "string" },
    "title":       { "type": ["string", "null"] },
    "content":     { "type": "string", "description": "Original Markdown/HTML as ingested. Clients are responsible for rendering." }
  }
}
```

**Errors:** `IndexNotMounted`, `SourceNotFound`, `SourceTypeNotIndexed`,
`ArtifactVersionMismatch`.

---

### 4. `get_asset`

Read a single asset from the bundled `assets.zip`. Typically JSON; may
be binary in future phases (returned base64-encoded with a content-type
hint).

**Status:** tool surface live in Phase 3. Returns `SourceTypeNotIndexed`
until Phase 4 ingestion lands.

**Input:**
```json
{
  "type": "object",
  "required": ["path"],
  "properties": {
    "path":     { "type": "string" },
    "build_id": { "type": "string" }
  }
}
```

**Output:**
```json
{
  "type": "object",
  "required": ["path", "build_id", "content_type", "content"],
  "properties": {
    "path":         { "type": "string" },
    "build_id":     { "type": "string" },
    "content_type": { "type": "string", "description": "MIME-ish. `application/json`, `text/plain`, `application/octet-stream`, etc." },
    "content":      { "type": "string", "description": "UTF-8 text when content_type starts with text/ or application/json. Otherwise base64." },
    "encoding":     { "type": "string", "enum": ["utf-8", "base64"] },
    "size_bytes":   { "type": "integer", "minimum": 0 }
  }
}
```

**Errors:** `IndexNotMounted`, `SourceNotFound`, `SourceTypeNotIndexed`.

---

### 5. `find_symbol`

Resolve a symbol (class, method, field) by fully-qualified name or
fuzzy signature match. Powered by `symbols.sqlite` FTS5. Used by the
Phase 5 diff tracker; available standalone so agents can answer
"where is `HytaleServer.SCHEDULED_EXECUTOR` defined?" without
round-tripping through `search`.

**Input:**
```json
{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "fqn":       { "type": "string", "description": "Exact match on fully-qualified name." },
    "signature": { "type": "string", "description": "FTS5 fuzzy match on method signature." },
    "kind":      { "type": "string", "enum": ["class", "method", "field"] },
    "limit":     { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
    "build_id":  { "type": "string" }
  },
  "oneOf": [
    { "required": ["fqn"] },
    { "required": ["signature"] }
  ]
}
```

**Output:**
```json
{
  "type": "object",
  "required": ["matches", "build_id"],
  "properties": {
    "build_id": { "type": "string" },
    "matches": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["kind", "fqn", "path"],
        "properties": {
          "kind":       { "type": "string", "enum": ["class", "method", "field"] },
          "fqn":        { "type": "string" },
          "signature":  { "type": ["string", "null"], "description": "Method param/return signature; null for classes/fields." },
          "declaring_class": { "type": ["string", "null"] },
          "modifiers":  { "type": "array", "items": { "type": "string" } },
          "path":       { "type": "string" },
          "start_line": { "type": ["integer", "null"], "minimum": 1 },
          "end_line":   { "type": ["integer", "null"], "minimum": 1 }
        }
      }
    }
  }
}
```

**Errors:** `IndexNotMounted`, `SymbolNotFound`, `InvalidQuery`,
`ArtifactVersionMismatch`.

---

## Error taxonomy

All errors return MCP's standard error envelope with a stable
`code` string in `error.data.code`. The message field is
human-readable and may change between versions - clients must switch
on `code`.

| Code                         | Meaning                                                                                             | Retryable |
|------------------------------|-----------------------------------------------------------------------------------------------------|-----------|
| `IndexNotMounted`            | No artifact is mounted. Desktop: user hasn't fetched one yet. Serve: catalog is empty.              | No        |
| `ArtifactVersionMismatch`    | Explicit `build_id` is not mounted.                                                                 | No        |
| `SourceNotFound`             | `path` is not in the artifact.                                                                      | No        |
| `SourceTypeNotIndexed`       | Artifact doesn't include that source type (e.g., Phase-3 artifacts return this for `hm_doc`).      | No        |
| `SymbolNotFound`             | `find_symbol` with `fqn` got no exact match; with `signature` got no FTS5 hit above threshold.      | No        |
| `InvalidQuery`               | Query is syntactically broken (empty, exceeds max length, malformed filter).                        | No        |
| `RateLimited`                | Hosted only. Includes `retry_after_seconds` in `error.data`.                                        | Yes       |
| `Internal`                   | Catch-all for unexpected server errors. Server logs carry a trace id echoed in `error.data.trace`.  | Maybe     |

`error.data` always includes `code` and may include:
- `trace`        - server-side trace id (for support)
- `build_id`     - the build-id the request was routed to (useful when
                   `ArtifactVersionMismatch` fires with a default build)
- `retry_after_seconds` - `RateLimited` only

## Partial availability

When an artifact is mounted but a dependency fails (e.g., Lance vector
store missing → keyword-only search), the tool succeeds and sets
`partial: true` in the response. Clients can choose to show a banner
or silently proceed. No error is raised in this case - partial results
are better than none, and the client has everything it needs to decide.

## Catalog tool (advisory)

MCP does not expose a `list_builds` or `select_build` tool in 1.0.
Rationale: the catalog is the desktop UX's concern, and agents asking
"what builds are available?" fall through to the server's default
routing. Revisit in 2.0 if we see repeated requests.

## Contract compliance

`src-tauri/tests/mcp_contract.rs` (Phase 3.I.3) validates every tool's
actual input/output against the JSON Schemas in this document. The
same suite runs against `atlas-serve` when that lands - divergence
between local and hosted is a test failure, not a documentation bug.
