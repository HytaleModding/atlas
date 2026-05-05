//! MCP tool surface.
//!
//! Implements the tools defined in `docs/mcp-contract.md` on top of the
//! local `SearchCatalog` + `SharedEmbedder` + ProjectDirs. The same
//! `AtlasTools` service type is reused by the hosted `atlas-serve` binary.
//!
//! Transport is streamable HTTP, mounted at `/mcp` on the existing Axum
//! router (`http::router`). The factory closure in [`build_mcp_service`]
//! clones the injected state per connection, so every session sees the
//! same Arcs without re-resolving ProjectDirs.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{
    handler::server::{
        router::tool::ToolRouter,
        wrapper::{Json, Parameters},
    },
    model::{ErrorCode, ErrorData, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpService,
    },
    ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Slot;
use crate::embedder::SharedEmbedder;
use crate::indexer::{self, symbols::SymbolKind, SearchCatalog, SearchHit};
use crate::lance;

/// Shared backend state the MCP tool handlers need. Cloned cheaply
/// because everything inside is either an `Arc<_>` or an owned
/// `PathBuf`.
#[derive(Clone)]
pub struct McpState {
    pub catalog: Arc<SearchCatalog>,
    pub embedder: Arc<SharedEmbedder>,
    pub data_dir: PathBuf,
}

/// Tool service. Holds a snapshot of [`McpState`] taken at service
/// construction; the streamable HTTP transport creates one instance per
/// session via the factory in [`build_mcp_service`].
#[derive(Clone)]
pub struct AtlasTools {
    state: McpState,
    // Held so the `#[tool_router]` macro's generated dispatch table stays
    // alive for this instance. Not read directly - the macro wires it up.
    #[allow(dead_code)]
    tool_router: ToolRouter<AtlasTools>,
}

impl AtlasTools {
    pub fn new(state: McpState) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }
}

// Parameter / output schemas. The `#[derive(JsonSchema)]` attributes let
// rmcp auto-publish these in the MCP `list_tools` response, which keeps
// the runtime surface aligned with docs/mcp-contract.md.

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Natural-language or keyword query.
    pub query: String,
    /// Cap on results. Server clamps to [1, 100]; default 25.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Restrict to one or more source types. Source types that are
    /// not yet ingested on this build return empty hit lists.
    #[serde(default)]
    pub source_type: Option<SourceTypeFilter>,
    #[serde(default)]
    pub build_id: Option<String>,
}

/// Either a single source-type string or a list of them. Mirrors the
/// `oneOf` schema in the contract.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SourceTypeFilter {
    One(String),
    Many(Vec<String>),
}

impl SourceTypeFilter {
    /// Flatten to a `Vec<String>` for downstream filters. Both Tantivy
    /// (BooleanQuery on the `source_type` field) and Lance (`only_if`
    /// SQL predicate) accept the same shape.
    fn as_vec(&self) -> Vec<String> {
        match self {
            SourceTypeFilter::One(s) => vec![s.clone()],
            SourceTypeFilter::Many(list) => list.clone(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchOutput {
    pub build_id: String,
    pub query: String,
    pub elapsed_ms: u64,
    /// True when vector search is unavailable and results are
    /// keyword-only. See `docs/mcp-contract.md` § Partial availability.
    pub partial: bool,
    pub hits: Vec<SearchHitOut>,
}

/// Flat form of `indexer::SearchHit` with the `source_type`
/// discriminator attached. Kept separate from the in-process `SearchHit`
/// so the wire shape can evolve without dragging the indexer along.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchHitOut {
    pub source_type: String,
    pub path: String,
    pub fqn: String,
    pub package: String,
    pub filename: String,
    pub score: f32,
    pub line_count: u64,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
    pub preview_line: Option<usize>,
    pub preview: Option<String>,
    pub chunk_kind: String,
    pub symbol_name: String,
}

impl From<SearchHit> for SearchHitOut {
    fn from(hit: SearchHit) -> Self {
        SearchHitOut {
            // Propagated from the underlying Tantivy/Lance row. The
            // `hm_doc`/`hypixel_doc`/`asset` sections plumb this field
            // end-to-end so MCP consumers can distinguish hits by their
            // real section.
            source_type: hit.source_type,
            path: hit.path,
            fqn: hit.fqn,
            package: hit.package,
            filename: hit.filename,
            score: hit.score,
            line_count: hit.line_count,
            start_line: hit.start_line,
            end_line: hit.end_line,
            preview_line: hit.preview_line,
            preview: hit.preview,
            chunk_kind: hit.chunk_kind,
            symbol_name: hit.symbol_name,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSourceParams {
    pub path: String,
    #[serde(default)]
    pub build_id: Option<String>,
    #[serde(default)]
    pub start_line: Option<u64>,
    #[serde(default)]
    pub end_line: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetSourceOutput {
    pub path: String,
    pub build_id: String,
    pub content: String,
    pub line_count: u64,
    pub truncated: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDocParams {
    pub path: String,
    pub source_type: String,
    #[serde(default)]
    pub build_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetAssetParams {
    pub path: String,
    #[serde(default)]
    pub build_id: Option<String>,
}

/// Contract-shaped output for `get_doc`. The schemars-derived schema is
/// what clients see in `list_tools`, so it must match `docs/mcp-contract.md`
/// § `get_doc`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GetDocOutput {
    pub path: String,
    pub source_type: String,
    pub build_id: String,
    pub title: Option<String>,
    pub content: String,
}

/// Contract-shaped output for `get_asset`. The handler currently errors
/// with `SourceTypeNotIndexed` until asset ingestion is wired up.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GetAssetOutput {
    pub path: String,
    pub build_id: String,
    pub content_type: String,
    pub content: String,
    pub encoding: String,
    pub size_bytes: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindSymbolParams {
    #[serde(default)]
    pub fqn: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub build_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FindSymbolOutput {
    pub build_id: String,
    pub matches: Vec<SymbolMatch>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SymbolMatch {
    pub kind: String,
    pub fqn: String,
    pub signature: Option<String>,
    pub declaring_class: Option<String>,
    pub modifiers: Vec<String>,
    pub path: String,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
}

// Tool handlers.

#[tool_router(vis = "pub")]
impl AtlasTools {
    /// Hybrid keyword + semantic search. Falls back to keyword-only
    /// when the vector store or embedder is unavailable (sets
    /// `partial: true` in the response).
    #[tool(
        name = "search",
        description = "Hybrid keyword + semantic search over one mounted artifact."
    )]
    pub async fn search_tool(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<Json<SearchOutput>, ErrorData> {
        let query = params.query.trim();
        if query.is_empty() {
            return Err(contract_error("InvalidQuery", "query must be non-empty"));
        }
        if query.len() > 1024 {
            return Err(contract_error(
                "InvalidQuery",
                "query exceeds 1024-character maximum",
            ));
        }
        // Flatten the optional source-type filter into the shape
        // hybrid::run expects (`Option<Vec<String>>`). Empty list is
        // treated the same as `None`.
        let source_types: Option<Vec<String>> = params
            .source_type
            .as_ref()
            .map(|f| f.as_vec())
            .filter(|v| !v.is_empty());

        let slot = resolve_slot(&params.build_id)?;
        let build_id = slot_build_id(slot, &params.build_id);
        let limit = params.limit.unwrap_or(25).clamp(1, 100) as usize;

        let index_dir = indexer::index_dir_for(&self.state.data_dir, slot);
        let lance_dir = lance::lance_dir_for(&self.state.data_dir, slot);
        let model_cache = self.state.data_dir.join("models");

        let lance_store = match lance::LanceStore::open_existing(&lance_dir).await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(?err, "lance open failed for mcp search, keyword-only");
                None
            }
        };
        let embedder_instance: Option<Arc<dyn crate::embedder::Embedder>> = if lance_store.is_some()
        {
            match self.state.embedder.get_or_init(model_cache) {
                Ok(e) => Some(e),
                Err(err) => {
                    tracing::warn!(?err, "embedder load failed for mcp search");
                    None
                }
            }
        } else {
            None
        };
        let partial = embedder_instance.is_none();

        let start = std::time::Instant::now();
        let hits = crate::search::hybrid::run(
            self.state.catalog.clone(),
            lance_store,
            embedder_instance,
            slot,
            &index_dir,
            query,
            limit,
            source_types,
        )
        .await
        .map_err(|err| {
            // `IndexNotMounted` is the usual cause here; map the
            // common case to the contract code and fall through to
            // Internal for everything else.
            let msg = format!("{err:#}");
            if msg.contains("no index for") {
                contract_error("IndexNotMounted", &msg)
            } else {
                contract_error("Internal", &msg)
            }
        })?;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        Ok(Json(SearchOutput {
            build_id,
            query: query.to_string(),
            elapsed_ms,
            partial,
            hits: hits.into_iter().map(SearchHitOut::from).collect(),
        }))
    }

    /// Read a single source file from the artifact's decompile tree.
    #[tool(
        name = "get_source",
        description = "Read the full text of a source file from an artifact's decompile tree."
    )]
    pub async fn get_source_tool(
        &self,
        Parameters(params): Parameters<GetSourceParams>,
    ) -> Result<Json<GetSourceOutput>, ErrorData> {
        match (params.start_line, params.end_line) {
            (Some(_), None) | (None, Some(_)) => {
                return Err(contract_error(
                    "InvalidQuery",
                    "start_line and end_line must be set together",
                ));
            }
            (Some(start), Some(end)) if end < start => {
                return Err(contract_error(
                    "InvalidQuery",
                    "end_line must be >= start_line",
                ));
            }
            _ => {}
        }
        let slot = resolve_slot(&params.build_id)?;
        let build_id = slot_build_id(slot, &params.build_id);
        let decompile_dir =
            crate::patcher::workspace_for(&self.state.data_dir, slot).join("decompile");
        let full = indexer::read_source(&decompile_dir, &params.path)
            .map_err(|err| contract_error("SourceNotFound", &format!("{err:#}")))?;
        let line_count = full.lines().count() as u64;
        let (content, truncated) = match (params.start_line, params.end_line) {
            (Some(start), Some(end)) => {
                let start = start.max(1) as usize;
                let end = end as usize;
                let clipped = full
                    .lines()
                    .skip(start.saturating_sub(1))
                    .take(end.saturating_sub(start - 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                let truncated = (start as u64) > 1 || (end as u64) < line_count;
                (clipped, truncated)
            }
            _ => (full, false),
        };
        Ok(Json(GetSourceOutput {
            path: params.path,
            build_id,
            content,
            line_count,
            truncated,
        }))
    }

    /// Read an HM Modding markdown guide or Hypixel Javadoc page from
    /// the shared content cache. Mirrors the desktop `read_source`
    /// command so MCP clients see the same content the in-app viewer
    /// renders.
    #[tool(
        name = "get_doc",
        description = "Read a documentation page from the artifact. HM docs + Hypixel docs share this tool, dispatched by source_type."
    )]
    pub async fn get_doc_tool(
        &self,
        Parameters(params): Parameters<GetDocParams>,
    ) -> Result<Json<GetDocOutput>, ErrorData> {
        let slot = resolve_slot(&params.build_id)?;
        let build_id = slot_build_id(slot, &params.build_id);
        let cache = crate::cache_root();
        let base = match params.source_type.as_str() {
            "hm_doc" => cache.join("hm-docs").join("site"),
            "hypixel_doc" => cache.join("javadocs"),
            other => {
                return Err(contract_error(
                    "InvalidQuery",
                    &format!("unsupported source_type `{}` for get_doc", other),
                ));
            }
        };
        let raw = indexer::read_source(&base, &params.path)
            .map_err(|err| contract_error("SourceNotFound", &format!("{err:#}")))?;
        // Hypixel pages are HTML; render through the same parser the
        // indexer used so MCP clients receive prose, not markup soup.
        let content = if params.source_type == "hypixel_doc" {
            indexer::hypixel_docs::render_class_page(&params.path, &raw).unwrap_or(raw)
        } else {
            raw
        };
        Ok(Json(GetDocOutput {
            path: params.path,
            source_type: params.source_type,
            build_id,
            title: None,
            content,
        }))
    }

    /// Asset retrieval surface placeholder. Asset ingestion has not
    /// landed yet, so the tool reports `SourceTypeNotIndexed` even
    /// though the schema is published in `list_tools`.
    #[tool(
        name = "get_asset",
        description = "Read a single asset from the bundled assets.zip. Not yet available."
    )]
    pub async fn get_asset_tool(
        &self,
        Parameters(_): Parameters<GetAssetParams>,
    ) -> Result<Json<GetAssetOutput>, ErrorData> {
        Err(contract_error(
            "SourceTypeNotIndexed",
            "asset ingestion is not yet available on this build",
        ))
    }

    /// Resolve a symbol by FQN or fuzzy signature match. Reads from
    /// `<index_dir>/symbols.sqlite`.
    #[tool(
        name = "find_symbol",
        description = "Resolve a symbol (class/method/field) by FQN or fuzzy signature match. Powered by symbols.sqlite FTS5."
    )]
    pub async fn find_symbol_tool(
        &self,
        Parameters(params): Parameters<FindSymbolParams>,
    ) -> Result<Json<FindSymbolOutput>, ErrorData> {
        if params.fqn.is_none() && params.signature.is_none() {
            return Err(contract_error(
                "InvalidQuery",
                "one of `fqn` or `signature` is required",
            ));
        }
        let kind = params
            .kind
            .as_deref()
            .map(|k| {
                SymbolKind::from_str(k).ok_or_else(|| {
                    contract_error("InvalidQuery", &format!("unknown symbol kind `{k}`"))
                })
            })
            .transpose()?;

        let slot = resolve_slot(&params.build_id)?;
        let build_id = slot_build_id(slot, &params.build_id);
        let index_dir = indexer::index_dir_for(&self.state.data_dir, slot);
        let symbols_path = index_dir.join("symbols.sqlite");
        if !symbols_path.is_file() {
            return Err(contract_error(
                "IndexNotMounted",
                "no symbols sidecar at this build id (run the indexer first)",
            ));
        }
        let db = indexer::symbols::SymbolsDb::open_read_only(&symbols_path)
            .map_err(|err| contract_error("Internal", &format!("{err:#}")))?;

        let limit = params.limit.unwrap_or(10).clamp(1, 50) as usize;
        let hits = if let Some(fqn) = params.fqn.as_deref() {
            db.find_by_fqn(fqn, kind, limit)
        } else {
            db.find_by_signature(params.signature.as_deref().unwrap_or(""), limit)
        }
        .map_err(|err| contract_error("Internal", &format!("{err:#}")))?;

        if hits.is_empty() {
            return Err(contract_error(
                "SymbolNotFound",
                "no matching symbols for the given criteria",
            ));
        }

        let matches: Vec<SymbolMatch> = hits
            .into_iter()
            .map(|h| SymbolMatch {
                kind: h.kind.as_str().to_string(),
                fqn: h.fqn,
                signature: h.signature,
                declaring_class: h.declaring_class,
                modifiers: h.modifiers,
                path: h.rel_path,
                start_line: h.start_line,
                end_line: h.end_line,
            })
            .collect();

        Ok(Json(FindSymbolOutput { build_id, matches }))
    }
}

#[tool_handler]
impl ServerHandler for AtlasTools {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
        .with_instructions(
            "Atlas MCP surface. See docs/mcp-contract.md for the authoritative tool + error schemas.",
        );
        info.server_info = Implementation::new("atlas", env!("CARGO_PKG_VERSION"));
        info
    }
}

// Router integration.

/// Build the streamable-HTTP MCP service Tower layer for `/mcp`. The
/// factory closure runs per session; everything inside [`McpState`] is
/// cheap to clone.
pub fn build_mcp_service(
    state: McpState,
) -> StreamableHttpService<AtlasTools, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(AtlasTools::new(state.clone())),
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    )
}

// Helpers.

/// Construct an `ErrorData` whose `data.code` is the contract error
/// code (per `docs/mcp-contract.md` § Error taxonomy). rmcp stores
/// structured payloads in `data`, which is what clients switch on.
fn contract_error(code: &str, message: &str) -> ErrorData {
    let rpc_code = match code {
        "InvalidQuery" => ErrorCode::INVALID_PARAMS,
        "RateLimited" => ErrorCode(-32001),
        _ => ErrorCode::INTERNAL_ERROR,
    };
    ErrorData::new(
        rpc_code,
        message.to_string(),
        Some(serde_json::json!({ "code": code })),
    )
}

/// Map a contract `build_id` to the local `Slot` that currently
/// backs it. Today there are only two local slots (release /
/// pre-release); fetched artifacts under `<data>/indexes/<build_id>/`
/// aren't addressable through the slot-based Tantivy/Lance layout yet,
/// so we reject those with `ArtifactVersionMismatch` for now. A future
/// catalog lookup will replace this.
fn resolve_slot(build_id: &Option<String>) -> Result<Slot, ErrorData> {
    let Some(id) = build_id.as_deref() else {
        return Ok(active_slot());
    };
    // Build IDs are either bare slot names (`release`, `pre-release`) or
    // an artifact-style `<slot>-<version>` (e.g. `release-2026.03.26-89796e57b`).
    // Match on the slot prefix as a whole token, not a substring, so the
    // `pre-release` case is checked before `release` and there's no
    // possibility of an unrelated suffix matching by accident.
    let slot = if id == "pre-release" || id.starts_with("pre-release-") {
        Slot::PreRelease
    } else if id == "release" || id.starts_with("release-") {
        Slot::Release
    } else {
        return Err(contract_error(
            "ArtifactVersionMismatch",
            &format!("build_id `{id}` is not mounted on this client"),
        ));
    };
    Ok(slot)
}

/// Return the configured `active_branch` slot, falling back to
/// `Slot::Release` when the config file is missing or unreadable -
/// same fallback the desktop UI uses.
fn active_slot() -> Slot {
    match crate::config::load() {
        Ok(cfg) => cfg.active_branch,
        Err(_) => Slot::Release,
    }
}

/// Echo the caller's build_id verbatim when provided, otherwise report
/// the slot's canonical short id.
fn slot_build_id(slot: Slot, override_: &Option<String>) -> String {
    override_
        .clone()
        .unwrap_or_else(|| slot.as_str().to_string())
}
