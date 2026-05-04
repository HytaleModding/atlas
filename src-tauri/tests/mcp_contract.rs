//! contract tests.
//!
//! Validates that the MCP tool surface in `src/mcp/mod.rs` conforms to
//! the authoritative schema in `docs/mcp-contract.md`. Anything that
//! would silently drift between the runtime and the docs - tool names,
//! required parameters, output shape, error codes - gets caught here.
//!
//! The same suite is intended to run against `atlas-serve` when that
//! lands (Phase 8). Divergence between local and hosted MCP is a test
//! failure, not a documentation bug (per the contract document).
//!
//! These tests exercise the tool *metadata* (auto-derived JSON Schemas
//! + tool registry) and the error-taxonomy path (short-circuit
//! validation + Phase-4 stubs). Behavioural tests for the underlying
//! `search`, `read_source`, and `SymbolsDb` paths live in their own
//! module tests; calling those paths through the MCP layer would
//! require standing up a full index, which is out of scope here.

use atlas_lib::mcp::{
    AtlasTools, FindSymbolParams, GetAssetParams, GetDocParams, GetSourceParams, McpState,
    SearchParams,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ErrorCode;
use serde_json::Value;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fresh_tools() -> AtlasTools {
    AtlasTools::new(McpState {
        catalog: Arc::new(atlas_lib::indexer::SearchCatalog::new()),
        embedder: Arc::new(atlas_lib::embedder::SharedEmbedder::new()),
        data_dir: std::env::temp_dir().join("atlas-mcp-contract-test"),
    })
}

fn router() -> rmcp::handler::server::router::tool::ToolRouter<AtlasTools> {
    AtlasTools::tool_router()
}

fn input_schema(name: &str) -> Value {
    let r = router();
    let tool = r
        .get(name)
        .unwrap_or_else(|| panic!("tool `{name}` not registered"));
    serde_json::to_value(&*tool.input_schema).expect("input schema serializes to JSON")
}

fn assert_required(schema: &Value, tool: &str, field: &str) {
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("tool `{tool}` schema has no `required` array"));
    let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
    assert!(
        names.contains(&field),
        "tool `{tool}`: expected `{field}` in required list, got {names:?}"
    );
}

fn assert_property(schema: &Value, tool: &str, field: &str) {
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("tool `{tool}` schema has no `properties` object"));
    assert!(
        props.contains_key(field),
        "tool `{tool}`: expected property `{field}`, got {:?}",
        props.keys().collect::<Vec<_>>()
    );
}

/// Extract `error.data.code` - the contract-level code string every
/// error must carry. See `docs/mcp-contract.md` § Error taxonomy.
fn contract_code(err: &rmcp::model::ErrorData) -> Option<&str> {
    err.data.as_ref().and_then(|d| d.get("code")).and_then(Value::as_str)
}

// ---------------------------------------------------------------------------
// Tool registry - every tool in the contract must be registered, and
// the router must not carry anything not in the contract.
// ---------------------------------------------------------------------------

#[test]
fn registers_every_contract_tool() {
    let r = router();
    for name in ["search", "get_source", "get_doc", "get_asset", "find_symbol"] {
        assert!(
            r.has_route(name),
            "contract tool `{name}` is not registered on the MCP router"
        );
    }
}

#[test]
fn registers_no_extra_tools() {
    let names: Vec<String> = router()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    // `list_all` returns tools sorted by name.
    let expected = ["find_symbol", "get_asset", "get_doc", "get_source", "search"];
    assert_eq!(
        names, expected,
        "unexpected tool surface - contract drift against docs/mcp-contract.md"
    );
}

#[test]
fn every_tool_has_a_description() {
    for tool in router().list_all() {
        let desc = tool.description.as_deref().unwrap_or("");
        assert!(
            !desc.is_empty(),
            "tool `{}` is missing the description required by the contract",
            tool.name
        );
    }
}

// ---------------------------------------------------------------------------
// Per-tool input schema conformance.
// ---------------------------------------------------------------------------

#[test]
fn search_input_schema_matches_contract() {
    let s = input_schema("search");
    assert_required(&s, "search", "query");
    for optional in ["limit", "source_type", "build_id"] {
        assert_property(&s, "search", optional);
    }
}

#[test]
fn get_source_input_schema_matches_contract() {
    let s = input_schema("get_source");
    assert_required(&s, "get_source", "path");
    for optional in ["build_id", "start_line", "end_line"] {
        assert_property(&s, "get_source", optional);
    }
}

#[test]
fn get_doc_input_schema_matches_contract() {
    let s = input_schema("get_doc");
    assert_required(&s, "get_doc", "path");
    assert_required(&s, "get_doc", "source_type");
    assert_property(&s, "get_doc", "build_id");
}

#[test]
fn get_asset_input_schema_matches_contract() {
    let s = input_schema("get_asset");
    assert_required(&s, "get_asset", "path");
    assert_property(&s, "get_asset", "build_id");
}

#[test]
fn find_symbol_input_schema_matches_contract() {
    let s = input_schema("find_symbol");
    for optional in ["fqn", "signature", "kind", "limit", "build_id"] {
        assert_property(&s, "find_symbol", optional);
    }
    // Contract requires one-of `fqn`/`signature`. rmcp's `schemars`
    // derive emits an empty `required` (both fields are
    // `#[serde(default)]`); the runtime check in `find_symbol_tool`
    // enforces the one-of rule instead. Exercised by
    // `find_symbol_rejects_empty_input`.
}

// ---------------------------------------------------------------------------
// Error taxonomy. Each tool must encode its contract error code in
// `error.data.code` and pick the right JSON-RPC `code`. We exercise
// the short-circuit paths that don't require a mounted index.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_rejects_empty_query() {
    let tools = fresh_tools();
    let err = tools
        .search_tool(Parameters(SearchParams {
            query: "   ".into(),
            limit: None,
            source_type: None,
            build_id: None,
        }))
        .await
        .err()
        .expect("empty search query must error");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(contract_code(&err), Some("InvalidQuery"));
}

#[tokio::test]
async fn search_rejects_over_long_query() {
    let tools = fresh_tools();
    let long = "x".repeat(1025);
    let err = tools
        .search_tool(Parameters(SearchParams {
            query: long,
            limit: None,
            source_type: None,
            build_id: None,
        }))
        .await
        .err()
        .expect("over-long query must error");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(contract_code(&err), Some("InvalidQuery"));
}

#[tokio::test]
async fn search_short_circuits_non_source_filters() {
    // Contract: only ships `source_type = "source"`. A filter
    // that excludes `source` must return an empty hit list (not an
    // error) so clients can compose filters without special-casing.
    let tools = fresh_tools();
    let out = tools
        .search_tool(Parameters(SearchParams {
            query: "anything".into(),
            limit: None,
            source_type: Some(atlas_lib::mcp::SourceTypeFilter::One("hm_doc".into())),
            build_id: Some("release".into()),
        }))
        .await
        .expect("non-source filter must succeed with empty hits");
    let body = out.0;
    assert!(body.hits.is_empty(), "expected empty hits, got {:?}", body.hits);
    assert!(!body.partial, "partial must be false on a short-circuit");
    assert_eq!(body.query, "anything");
}

#[tokio::test]
async fn get_source_rejects_half_specified_line_range() {
    let tools = fresh_tools();
    let err = tools
        .get_source_tool(Parameters(GetSourceParams {
            path: "Foo.java".into(),
            build_id: None,
            start_line: Some(10),
            end_line: None,
        }))
        .await
        .err()
        .expect("half-specified range must error");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(contract_code(&err), Some("InvalidQuery"));
}

#[tokio::test]
async fn get_source_rejects_inverted_line_range() {
    let tools = fresh_tools();
    let err = tools
        .get_source_tool(Parameters(GetSourceParams {
            path: "Foo.java".into(),
            build_id: None,
            start_line: Some(42),
            end_line: Some(10),
        }))
        .await
        .err()
        .expect("inverted range must error");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(contract_code(&err), Some("InvalidQuery"));
}

#[tokio::test]
async fn get_doc_known_source_types_report_source_type_not_indexed() {
    let tools = fresh_tools();
    for source_type in ["hm_doc", "hypixel_doc"] {
        let err = tools
            .get_doc_tool(Parameters(GetDocParams {
                path: "README.md".into(),
                source_type: source_type.into(),
                build_id: None,
            }))
            .await
            .err()
            .unwrap_or_else(|| panic!("get_doc(`{source_type}`) must error in Phase 3"));
        assert_eq!(
            contract_code(&err),
            Some("SourceTypeNotIndexed"),
            "Phase 3 `{source_type}` must advertise SourceTypeNotIndexed",
        );
    }
}

#[tokio::test]
async fn get_doc_unknown_source_type_is_invalid_query() {
    let tools = fresh_tools();
    let err = tools
        .get_doc_tool(Parameters(GetDocParams {
            path: "x.md".into(),
            source_type: "not_a_real_type".into(),
            build_id: None,
        }))
        .await
        .err()
        .expect("unknown source_type must error");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(contract_code(&err), Some("InvalidQuery"));
}

#[tokio::test]
async fn get_asset_reports_source_type_not_indexed() {
    let tools = fresh_tools();
    let err = tools
        .get_asset_tool(Parameters(GetAssetParams {
            path: "something.json".into(),
            build_id: None,
        }))
        .await
        .err()
        .expect("get_asset must error in Phase 3");
    assert_eq!(contract_code(&err), Some("SourceTypeNotIndexed"));
}

#[tokio::test]
async fn find_symbol_rejects_empty_input() {
    let tools = fresh_tools();
    let err = tools
        .find_symbol_tool(Parameters(FindSymbolParams {
            fqn: None,
            signature: None,
            kind: None,
            limit: None,
            build_id: None,
        }))
        .await
        .err()
        .expect("find_symbol with neither fqn nor signature must error");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(contract_code(&err), Some("InvalidQuery"));
}

#[tokio::test]
async fn find_symbol_rejects_unknown_kind() {
    let tools = fresh_tools();
    let err = tools
        .find_symbol_tool(Parameters(FindSymbolParams {
            fqn: Some("com.example.Foo".into()),
            signature: None,
            kind: Some("interface".into()), // not one of class/method/field
            limit: None,
            build_id: None,
        }))
        .await
        .err()
        .expect("unknown kind must error");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(contract_code(&err), Some("InvalidQuery"));
}

#[tokio::test]
async fn build_id_that_is_not_release_or_pre_release_is_version_mismatch() {
    // Contract: an explicit build_id that isn't currently mounted must
    // come back as ArtifactVersionMismatch, not Internal. find_symbol
    // is the cleanest probe - it short-circuits on the resolver before
    // touching disk.
    let tools = fresh_tools();
    let err = tools
        .find_symbol_tool(Parameters(FindSymbolParams {
            fqn: Some("com.example.Foo".into()),
            signature: None,
            kind: None,
            limit: None,
            build_id: Some("some-random-build-id-12345".into()),
        }))
        .await
        .err()
        .expect("unmounted build_id must error");
    assert_eq!(contract_code(&err), Some("ArtifactVersionMismatch"));
}
