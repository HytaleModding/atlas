//! Tree-sitter based chunker for Java sources. Two chunk kinds are emitted:
//!
//! - **Type** chunk: one per top-level or nested class/interface/enum/record.
//! Text includes the class signature, its javadoc (if any), field
//! declarations, and method signatures (headers only, no bodies).
//! Acts as a "table of contents" for the class - good for queries like
//! `PageManager getComponent`.
//!
//! - **Method** chunk: one per method or constructor (including those in
//! nested classes). Text is prefixed with the enclosing class FQN and
//! (if short) its class javadoc so the embedding model has enough
//! context to tell similarly-named methods apart across the codebase.
//!
//! Best-effort: if tree-sitter can't parse a file, callers fall back to a
//! single file-level chunk so nothing gets silently dropped.
//!
//! Method bodies are included verbatim; the BGE-small window is 512 tokens
//! but most methods fit comfortably. Overflow handling is left for later -
//! the chunk contract stays simple here.

use tree_sitter::{Node, Parser};

/// The kind of symbol a chunk represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Type,
    Method,
 /// Constructor - syntactically similar to a method but worth
 /// distinguishing in the UI: a search for `PageManager` that hits
 /// `public PageManager(...)` should surface as "constructor …",
 /// not "method …", so the user understands what actually matched.
    Constructor,
 /// Fallback used when tree-sitter fails to parse the file.
    File,
}

impl ChunkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChunkKind::Type => "type",
            ChunkKind::Method => "method",
            ChunkKind::Constructor => "constructor",
            ChunkKind::File => "file",
        }
    }
}

/// One indexable slice of a Java source file.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub kind: ChunkKind,
 /// Simple symbol name. For types this is the class name; for methods,
 /// the method name (constructor names == class name).
    pub symbol_name: String,
 /// Dotted FQN of the enclosing class. For Type chunks this is the
 /// class's own FQN; for Method chunks it's the enclosing class's FQN.
    pub class_fqn: String,
 /// 1-based inclusive line range in the source file.
    pub start_line: u64,
    pub end_line: u64,
 /// Tokenized payload for Tantivy + (later) embedding model input.
    pub text: String,
}

/// What kind of Java type a [`ClassSymbol`] represents. Kept as a distinct
/// enum from [`ChunkKind`] because `ChunkKind::Type` collapses all of these
/// into one bucket (the embedding model doesn't care), whereas the symbol
/// sidecar needs the distinction for diff queries ("is this still an enum?").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Class,
    Interface,
    Enum,
    Record,
    Annotation,
}

impl TypeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TypeKind::Class => "class",
            TypeKind::Interface => "interface",
            TypeKind::Enum => "enum",
            TypeKind::Record => "record",
            TypeKind::Annotation => "annotation",
        }
    }
}

/// Structural description of a Java class/interface/enum/record.
///
/// Stored in `symbols.sqlite` during indexing. Feeds:
/// - diff tracker (resolve `import foo.Bar` against an older index)
/// - `find_symbol` MCP tool
/// - Index Catalog UX counts
#[derive(Debug, Clone)]
pub struct ClassSymbol {
    pub fqn: String,
    pub simple_name: String,
    pub kind: TypeKind,
 /// Access + non-access modifiers in source order: `public`, `abstract`,
 /// `static`, `final`, `sealed`, etc. Raw tokens - don't normalize
 /// because the SQLite side just round-trips them.
    pub modifiers: Vec<String>,
 /// FQN of the direct superclass, if one is declared explicitly.
 /// For interfaces this is always None; for classes with no `extends`
 /// clause this is None too (implicit `java.lang.Object`).
    pub superclass: Option<String>,
 /// Implemented interfaces (for classes) / extended interfaces (for
 /// interfaces). Not resolved against imports - raw source text.
    pub interfaces: Vec<String>,
    pub start_line: u64,
    pub end_line: u64,
}

/// Structural description of a method or constructor.
#[derive(Debug, Clone)]
pub struct MethodSymbol {
    pub class_fqn: String,
 /// Method name. For constructors, equal to the enclosing class's
 /// simple name (`PageManager`).
    pub name: String,
    pub is_constructor: bool,
    pub modifiers: Vec<String>,
 /// Return type as it appears in source. `None` for constructors.
 /// Generics preserved verbatim (`List<Map<String, Integer>>`).
    pub return_type: Option<String>,
 /// Parameter types in declaration order, as raw source text. Includes
 /// generics. Does not include parameter names - unused in diff-tracker
 /// lookups.
    pub param_types: Vec<String>,
 /// `throws` clause FQNs (unresolved, as written).
    pub thrown: Vec<String>,
    pub start_line: u64,
    pub end_line: u64,
}

/// Structural description of a field or enum constant.
#[derive(Debug, Clone)]
pub struct FieldSymbol {
    pub class_fqn: String,
    pub name: String,
 /// Declared type as source text. For enum constants this is the
 /// enclosing enum's simple name - callers who care about enum-vs-field
 /// can cross-reference `ClassSymbol.kind`.
    pub type_text: String,
    pub modifiers: Vec<String>,
    pub start_line: u64,
    pub end_line: u64,
}

/// All structural symbols extracted from a single file. Populated alongside
/// `chunks` in [`chunk_file`]; the indexer writes these to `symbols.sqlite`
/// in the same pass.
#[derive(Debug, Clone, Default)]
pub struct FileSymbols {
    pub classes: Vec<ClassSymbol>,
    pub methods: Vec<MethodSymbol>,
    pub fields: Vec<FieldSymbol>,
}

/// Combined return of the chunker's single tree-sitter pass - search-oriented
/// chunks on one side, structural metadata on the other. Callers that only
/// want chunks can use [`chunk_file`] directly; it discards the symbols.
#[derive(Debug, Clone, Default)]
pub struct ChunkerOutput {
    pub chunks: Vec<Chunk>,
    pub symbols: FileSymbols,
 /// The dotted package taken from the source file's `package` declaration
 /// - authoritative when present, since decompiled trees can strip leading
 /// directory segments (Vineflower's hytale output drops `com/hypixel/`)
 /// while the in-file declaration stays intact. `None` when the file has
 /// no `package` line (default package, parse failure, or `package-info.java`
 /// without one).
    pub parsed_package: Option<String>,
}

/// Upper bound on javadoc text prepended to method chunks. Anything longer
/// is skipped - don't want one fat class-level comment to eat the
/// 512-token embedding window for every method.
const JAVADOC_PREFIX_CAP: usize = 300;

/// Parse a Java source file and return its chunks. The `package` argument
/// is a fallback dotted package (typically derived from the file's path);
/// the chunker prefers the source's actual `package` declaration when one
/// is present and only falls back to the argument otherwise.
///
/// Returns a single `File` chunk when parsing fails OR when the file
/// contains no top-level types (unusual, but e.g. package-info.java).
///
/// This is a thin wrapper around [`chunk_and_extract`] for callers that
/// only care about chunks. Symbol metadata is discarded.
pub fn chunk_file(source: &str, package: &str) -> Vec<Chunk> {
    chunk_and_extract(source, package).chunks
}

/// Single-pass parse: returns both the chunks (for Tantivy + Lance) and the
/// structural symbols (for the SQLite sidecar). Share one tree-sitter
/// walk because both consumers want the same nodes - tree walks are cheap
/// compared to embedding but doing them twice is still wasted work.
///
/// `package_fallback` is used only when the source file has no parseable
/// `package` declaration. The in-file declaration is authoritative because
/// the decompile output can have its directory tree truncated (e.g.
/// Vineflower drops `com/hypixel/` from hytale's tree) while keeping the
/// declaration intact.
pub fn chunk_and_extract(source: &str, package_fallback: &str) -> ChunkerOutput {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_java::language()).is_err() {
        return ChunkerOutput {
            chunks: vec![whole_file_chunk(source, package_fallback)],
            symbols: FileSymbols::default(),
            parsed_package: None,
        };
    }
    let Some(tree) = parser.parse(source, None) else {
        return ChunkerOutput {
            chunks: vec![whole_file_chunk(source, package_fallback)],
            symbols: FileSymbols::default(),
            parsed_package: None,
        };
    };
    let bytes = source.as_bytes();
    let parsed_package = extract_package_declaration(tree.root_node(), bytes);
    let effective_package: &str = parsed_package
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(package_fallback);
    let mut out = ChunkerOutput {
        parsed_package: parsed_package.clone(),
        ..ChunkerOutput::default()
    };
    walk(tree.root_node(), bytes, effective_package, &[], &mut out);
    if out.chunks.is_empty() {
        out.chunks.push(whole_file_chunk(source, effective_package));
    }
    out
}

/// Pull the dotted name out of the file's `package` declaration. Returns
/// `None` when the file has no declaration (default package).
fn extract_package_declaration(root: Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "package_declaration" {
 // The dotted name is the first scoped_identifier or identifier
 // child. Annotations on the package decl precede it.
            let mut inner = child.walk();
            for n in child.named_children(&mut inner) {
                match n.kind() {
                    "scoped_identifier" | "identifier" => {
                        return n
                            .utf8_text(bytes)
                            .ok()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty());
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

fn whole_file_chunk(source: &str, package: &str) -> Chunk {
    let line_count = source.lines().count().max(1) as u64;
    Chunk {
        kind: ChunkKind::File,
        symbol_name: String::new(),
        class_fqn: package.to_string(),
        start_line: 1,
        end_line: line_count,
        text: source.to_string(),
    }
}

fn walk(
    node: Node,
    bytes: &[u8],
    package: &str,
    class_stack: &[String],
    out: &mut ChunkerOutput,
) {
    match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "annotation_type_declaration" => {
            visit_type(node, bytes, package, class_stack, out);
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk(child, bytes, package, class_stack, out);
            }
        }
    }
}

fn visit_type(
    node: Node,
    bytes: &[u8],
    package: &str,
    class_stack: &[String],
    out: &mut ChunkerOutput,
) {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| node_text(n, bytes))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let mut new_stack: Vec<String> = class_stack.to_vec();
    new_stack.push(name.clone());
    let class_fqn = build_fqn(package, &new_stack);
    let class_javadoc = preceding_javadoc(node, bytes);

 // Header = modifiers + `class Foo extends ... implements ...` up to body.
    let signature = type_signature(node, bytes);

 // Symbol metadata for the sidecar. Populated in parallel with the
 // Type chunk's TOC text below.
    let type_kind = type_kind_from_node(node);
    let modifiers = extract_modifiers(node, bytes);
    let superclass = extract_superclass(node, bytes);
    let interfaces = extract_interfaces(node, bytes);

 // --- build the Type chunk text as walk the body -----------------
    let mut type_text = String::new();
    if let Some(jd) = &class_javadoc {
        type_text.push_str(jd);
        type_text.push('\n');
    }
    type_text.push_str(&signature);
    type_text.push_str(" {\n");

    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            match child.kind() {
                "method_declaration" | "constructor_declaration" => {
                    let is_constructor = child.kind() == "constructor_declaration";
                    let chunk_kind = if is_constructor {
                        ChunkKind::Constructor
                    } else {
                        ChunkKind::Method
                    };
                    let method_name = child
                        .child_by_field_name("name")
                        .and_then(|n| node_text(n, bytes))
                        .unwrap_or_else(|| {
 // Constructors don't have a "name" field in
 // tree-sitter-java - they have a `type` field.
                            child
                                .child_by_field_name("type")
                                .and_then(|n| node_text(n, bytes))
                                .unwrap_or_else(|| "<init>".to_string())
                        });

                    let method_sig = method_signature(child, bytes);

 // Append to type-chunk TOC.
                    type_text.push_str("  ");
                    type_text.push_str(method_sig.trim());
                    type_text.push_str(";\n");

 // Emit the method / constructor chunk.
                    let body_text = slice_text(child, bytes);
                    let mut method_text = String::new();
                    method_text.push_str(&class_fqn);
                    method_text.push('\n');
                    if let Some(jd) = &class_javadoc {
                        if jd.len() <= JAVADOC_PREFIX_CAP {
                            method_text.push_str(jd);
                            method_text.push('\n');
                        }
                    }
                    method_text.push_str(body_text.trim());

                    let start_line = (child.start_position().row as u64) + 1;
                    let end_line = (child.end_position().row as u64) + 1;

                    out.chunks.push(Chunk {
                        kind: chunk_kind,
                        symbol_name: method_name.clone(),
                        class_fqn: class_fqn.clone(),
                        start_line,
                        end_line,
                        text: method_text,
                    });

 // And emit the symbol record.
                    out.symbols.methods.push(MethodSymbol {
                        class_fqn: class_fqn.clone(),
                        name: method_name,
                        is_constructor,
                        modifiers: extract_modifiers(child, bytes),
                        return_type: if is_constructor {
                            None
                        } else {
                            child
                                .child_by_field_name("type")
                                .and_then(|n| node_text(n, bytes))
                        },
                        param_types: extract_param_types(child, bytes),
                        thrown: extract_thrown(child, bytes),
                        start_line,
                        end_line,
                    });
                }
                "field_declaration" | "constant_declaration" => {
                    type_text.push_str("  ");
                    type_text.push_str(slice_text(child, bytes).trim());
                    type_text.push('\n');

                    let field_type = child
                        .child_by_field_name("type")
                        .and_then(|n| node_text(n, bytes))
                        .unwrap_or_default();
                    let mods = extract_modifiers(child, bytes);
 // A single declaration may define multiple fields
 // (`int a, b, c;`) - emit one record per declarator.
                    let mut cursor2 = child.walk();
                    for decl in child.named_children(&mut cursor2) {
                        if decl.kind() == "variable_declarator" {
                            let field_name = decl
                                .child_by_field_name("name")
                                .and_then(|n| node_text(n, bytes))
                                .unwrap_or_default();
                            if field_name.is_empty() {
                                continue;
                            }
                            out.symbols.fields.push(FieldSymbol {
                                class_fqn: class_fqn.clone(),
                                name: field_name,
                                type_text: field_type.clone(),
                                modifiers: mods.clone(),
                                start_line: (decl.start_position().row as u64) + 1,
                                end_line: (decl.end_position().row as u64) + 1,
                            });
                        }
                    }
                }
                "enum_constant" => {
                    type_text.push_str("  ");
                    type_text.push_str(slice_text(child, bytes).trim());
                    type_text.push_str(",\n");

                    let const_name = child
                        .child_by_field_name("name")
                        .and_then(|n| node_text(n, bytes))
                        .unwrap_or_default();
                    if !const_name.is_empty() {
                        out.symbols.fields.push(FieldSymbol {
                            class_fqn: class_fqn.clone(),
                            name: const_name,
 // For enum constants the "type" is the
 // enclosing enum's simple name.
                            type_text: name.clone(),
                            modifiers: vec![
                                "public".to_string(),
                                "static".to_string(),
                                "final".to_string(),
                            ],
                            start_line: (child.start_position().row as u64) + 1,
                            end_line: (child.end_position().row as u64) + 1,
                        });
                    }
                }
                "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration" => {
 // Nested type - record its symbol in the outer TOC and
 // recurse so it gets its own Type + Method chunks.
                    let nested_name = child
                        .child_by_field_name("name")
                        .and_then(|n| node_text(n, bytes))
                        .unwrap_or_default();
                    if !nested_name.is_empty() {
                        type_text.push_str("  // nested ");
                        type_text.push_str(&nested_name);
                        type_text.push('\n');
                    }
                    visit_type(child, bytes, package, &new_stack, out);
                }
                _ => {}
            }
        }
    }
    type_text.push('}');

    let type_start = (node.start_position().row as u64) + 1;
    let type_end = (node.end_position().row as u64) + 1;

    out.chunks.push(Chunk {
        kind: ChunkKind::Type,
        symbol_name: name.clone(),
        class_fqn: class_fqn.clone(),
        start_line: type_start,
        end_line: type_end,
        text: type_text,
    });

    out.symbols.classes.push(ClassSymbol {
        fqn: class_fqn,
        simple_name: name,
        kind: type_kind,
        modifiers,
        superclass,
        interfaces,
        start_line: type_start,
        end_line: type_end,
    });
}

// -----------------------------------------------------------------------
// helpers
// -----------------------------------------------------------------------

fn type_kind_from_node(node: Node) -> TypeKind {
    match node.kind() {
        "interface_declaration" => TypeKind::Interface,
        "enum_declaration" => TypeKind::Enum,
        "record_declaration" => TypeKind::Record,
        "annotation_type_declaration" => TypeKind::Annotation,
        _ => TypeKind::Class,
    }
}

/// Pull modifier tokens (`public`, `static`, `final`, etc.) from the
/// `modifiers` child of a class/method/field node. Raw source tokens -
/// no normalization. Returns empty when the node has no modifiers block
/// (e.g., package-private with no explicit modifiers).
///
/// Annotations that appear in the modifiers block (like `@Override`) are
/// skipped - diff-tracker checks care about access/non-access keywords, and
/// annotations would add noise without helping. If annotation info is ever
/// needed, a separate field on the symbol is the right shape.
fn extract_modifiers(node: Node, bytes: &[u8]) -> Vec<String> {
    let Some(mods) = node.child_by_field_name("modifiers") else {
 // tree-sitter-java doesn't always expose `modifiers` as a named
 // field; fall back to scanning named children.
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "modifiers" {
                return collect_modifier_tokens(child, bytes);
            }
        }
        return Vec::new();
    };
    collect_modifier_tokens(mods, bytes)
}

fn collect_modifier_tokens(mods: Node, bytes: &[u8]) -> Vec<String> {
    let mut cursor = mods.walk();
    let mut out = Vec::new();
    for child in mods.children(&mut cursor) {
 // Annotations live here too (`marker_annotation`, `annotation`);
 // skip them - only want keyword modifiers.
        if child.is_named() && child.kind().ends_with("annotation") {
            continue;
        }
        if let Some(text) = node_text(child, bytes) {
            let trimmed = text.trim();
 // Keep only simple keyword tokens. Anything with whitespace or
 // punctuation isn't a modifier.
            if !trimmed.is_empty()
                && trimmed.chars().all(|c| c.is_ascii_alphabetic())
            {
                out.push(trimmed.to_string());
            }
        }
    }
    out
}

/// Return the FQN-as-written of the class's declared superclass, if any.
/// `null` for interfaces, enums, records, annotations, and classes with
/// no explicit `extends`.
fn extract_superclass(node: Node, bytes: &[u8]) -> Option<String> {
    if node.kind() != "class_declaration" {
        return None;
    }
 // tree-sitter-java exposes `superclass` as a field on class_declaration.
 // It wraps the `extends` keyword + the type; want just the type text.
    let superclass = node.child_by_field_name("superclass")?;
 // The superclass node contains `extends` and a type child.
    let mut cursor = superclass.walk();
    for child in superclass.named_children(&mut cursor) {
        if let Some(text) = node_text(child, bytes) {
            return Some(text.trim().to_string());
        }
    }
    None
}

/// Return the list of implemented (for classes) or extended (for interfaces)
/// types as they appear in source. Raw text - unresolved against imports.
///
/// Tree-sitter-java exposes the wrapper as a *node kind* (`extends_interfaces`
/// for interfaces, `super_interfaces` for classes) - NOT as a field name.
/// Therefore walk named children and match by `kind()`, not `child_by_field_name`.
/// Inside the wrapper, the actual types live in a `type_list`.
fn extract_interfaces(node: Node, bytes: &[u8]) -> Vec<String> {
    let wrapper_kind = if node.kind() == "interface_declaration" {
        "extends_interfaces"
    } else {
        "super_interfaces"
    };

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != wrapper_kind {
            continue;
        }
        let mut c2 = child.walk();
        for inner in child.named_children(&mut c2) {
            if inner.kind() == "type_list" {
                let mut out = Vec::new();
                let mut c3 = inner.walk();
                for t in inner.named_children(&mut c3) {
                    if let Some(text) = node_text(t, bytes) {
                        out.push(text.trim().to_string());
                    }
                }
                return out;
            }
        }
    }
    Vec::new()
}

/// Extract parameter types (raw source text, generics preserved) from a
/// method_declaration or constructor_declaration node. Parameter names are
/// discarded - lookups in `symbols.sqlite` are on (class_fqn, name, types).
fn extract_param_types(node: Node, bytes: &[u8]) -> Vec<String> {
    let Some(params) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = params.walk();
    for p in params.named_children(&mut cursor) {
        match p.kind() {
            "formal_parameter" => {
                if let Some(ty) = p.child_by_field_name("type") {
                    if let Some(text) = node_text(ty, bytes) {
                        out.push(text.trim().to_string());
                    }
                }
            }
            "spread_parameter" => {
 // Unlike `formal_parameter`, tree-sitter-java does NOT
 // expose `type` as a field on `spread_parameter`. The type
 // appears as the first named child (type_identifier,
 // generic_type, array_type, etc.); `variable_declarator`
 // holds the parameter name and comes last.
                let mut c2 = p.walk();
                for t in p.named_children(&mut c2) {
                    if t.kind() == "variable_declarator" {
                        continue;
                    }
                    if let Some(text) = node_text(t, bytes) {
                        out.push(format!("{}...", text.trim()));
                    }
                    break;
                }
            }
            "receiver_parameter" => {
 // `this`-receiver pseudo-parameter. Not part of the JVM
 // signature; skip.
            }
            _ => {}
        }
    }
    out
}

/// Extract the `throws` clause FQNs (as written) from a method or
/// constructor node. Empty when no clause.
fn extract_thrown(node: Node, bytes: &[u8]) -> Vec<String> {
 // tree-sitter-java represents throws as a sibling `throws` node
 // inside the declaration, not as a field. Walk children.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "throws" {
            let mut out = Vec::new();
            let mut c2 = child.walk();
            for t in child.named_children(&mut c2) {
                if let Some(text) = node_text(t, bytes) {
                    out.push(text.trim().to_string());
                }
            }
            return out;
        }
    }
    Vec::new()
}

fn build_fqn(package: &str, class_stack: &[String]) -> String {
    let tail = class_stack.join(".");
    if package.is_empty() {
        tail
    } else {
        format!("{package}.{tail}")
    }
}

fn node_text(node: Node, bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()])
        .ok()
        .map(|s| s.to_string())
}

fn slice_text(node: Node, bytes: &[u8]) -> String {
    std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()])
        .unwrap_or("")
        .to_string()
}

/// Return the class/method signature text: everything from the declaration's
/// start up to (but not including) the body. Trimmed of trailing whitespace.
fn type_signature(node: Node, bytes: &[u8]) -> String {
    let end = node
        .child_by_field_name("body")
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    std::str::from_utf8(&bytes[node.start_byte()..end])
        .unwrap_or("")
        .trim()
        .to_string()
}

fn method_signature(node: Node, bytes: &[u8]) -> String {
 // Abstract methods have no body; fall back to the whole decl.
    let end = node
        .child_by_field_name("body")
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    std::str::from_utf8(&bytes[node.start_byte()..end])
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Find the javadoc that immediately precedes a declaration, if one exists.
/// Returns the raw comment text (including `/** ... */`), trimmed.
fn preceding_javadoc(node: Node, bytes: &[u8]) -> Option<String> {
    let prev = node.prev_named_sibling()?;
    if prev.kind() != "block_comment" {
        return None;
    }
    let text = node_text(prev, bytes)?;
    let trimmed = text.trim();
    if !trimmed.starts_with("/**") {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(chunks: &'a [Chunk], kind: ChunkKind, name: &str) -> &'a Chunk {
        chunks
            .iter()
            .find(|c| c.kind == kind && c.symbol_name == name)
            .unwrap_or_else(|| panic!("no {:?} chunk named {name}", kind))
    }

    #[test]
    fn parsed_package_overrides_path_fallback() {
 // Decompiler output that strips part of the directory tree but
 // keeps the in-file declaration. Worker passes the path-derived
 // package as fallback; chunker should prefer the declared one.
        let src = r#"
package com.hypixel.hytale.event;

public class EventBus {
    public void shutdown() {}
}
"#;
        let out = chunk_and_extract(src, "hytale.event");
        assert_eq!(
            out.parsed_package.as_deref(),
            Some("com.hypixel.hytale.event")
        );
        let bus = out
            .chunks
            .iter()
            .find(|c| c.kind == ChunkKind::Type && c.symbol_name == "EventBus")
            .expect("Type chunk for EventBus");
        assert_eq!(bus.class_fqn, "com.hypixel.hytale.event.EventBus");
    }

    #[test]
    fn missing_package_falls_back_to_argument() {
 // Default-package source: chunker has nothing to parse, so the
 // path-derived fallback wins.
        let src = "public class A { public void x() {} }\n";
        let out = chunk_and_extract(src, "fallback");
        assert_eq!(out.parsed_package, None);
        let a = out
            .chunks
            .iter()
            .find(|c| c.kind == ChunkKind::Type && c.symbol_name == "A")
            .expect("Type chunk for A");
        assert_eq!(a.class_fqn, "fallback.A");
    }

    #[test]
    fn simple_class_emits_type_and_method_chunks() {
        let src = r#"
package com.example;

public class Foo {
    public int bar() { return 1; }
    public void baz(String s) { System.out.println(s); }
}
"#;
        let chunks = chunk_file(src, "com.example");
        let kinds: Vec<ChunkKind> = chunks.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&ChunkKind::Type));
        assert_eq!(
            kinds.iter().filter(|k| **k == ChunkKind::Method).count(),
            2,
            "expected one Method chunk per method; got {chunks:?}"
        );

        let foo = find(&chunks, ChunkKind::Type, "Foo");
        assert_eq!(foo.class_fqn, "com.example.Foo");

        let bar = find(&chunks, ChunkKind::Method, "bar");
        assert_eq!(bar.class_fqn, "com.example.Foo");
 // Method text is prefixed with the enclosing class FQN so the
 // embedding model can disambiguate same-named methods.
        assert!(bar.text.starts_with("com.example.Foo"));
 // And contains the method body verbatim.
        assert!(bar.text.contains("return 1"));
    }

    #[test]
    fn type_chunk_is_a_toc_not_full_bodies() {
        let src = r#"
package com.example;
public class Foo {
    public int bar() { return 42; }
}
"#;
        let chunks = chunk_file(src, "com.example");
        let foo = find(&chunks, ChunkKind::Type, "Foo");
 // Signature line is in the TOC.
        assert!(foo.text.contains("public int bar()"));
 // Body is NOT - the Type chunk is an index/overview, bodies live
 // in their own Method chunks so similarity search hits the right one.
        assert!(!foo.text.contains("return 42"));
    }

    #[test]
    fn constructor_is_distinct_from_method() {
        let src = r#"
package com.example;
public class PageManager {
    public PageManager(int initialSize) { this.size = initialSize; }
    public int getSize() { return size; }
    private int size;
}
"#;
        let chunks = chunk_file(src, "com.example");
 // Both should be emitted, tagged differently, with matching
 // symbol names (constructor name == class name in Java).
        let ctor = find(&chunks, ChunkKind::Constructor, "PageManager");
        assert_eq!(ctor.class_fqn, "com.example.PageManager");
        assert!(ctor.text.contains("this.size = initialSize"));

        let method = find(&chunks, ChunkKind::Method, "getSize");
        assert_eq!(method.class_fqn, "com.example.PageManager");

 // And no constructor chunk accidentally gets ChunkKind::Method.
        assert!(
            !chunks
                .iter()
                .any(|c| c.kind == ChunkKind::Method && c.symbol_name == "PageManager"),
            "constructor leaked into Method kind: {chunks:?}"
        );
    }

    #[test]
    fn nested_class_fqn_includes_outer() {
        let src = r#"
package com.example;
public class Outer {
    public static class Inner {
        public void tick() {}
    }
}
"#;
        let chunks = chunk_file(src, "com.example");
        let inner = find(&chunks, ChunkKind::Type, "Inner");
        assert_eq!(inner.class_fqn, "com.example.Outer.Inner");

        let tick = find(&chunks, ChunkKind::Method, "tick");
        assert_eq!(tick.class_fqn, "com.example.Outer.Inner");
    }

    #[test]
    fn class_javadoc_is_prepended_to_type_chunk() {
        let src = r#"
package com.example;
/** Handles the widget lifecycle. */
public class Widget {
    public void render() {}
}
"#;
        let chunks = chunk_file(src, "com.example");
        let widget = find(&chunks, ChunkKind::Type, "Widget");
        assert!(widget.text.contains("Handles the widget lifecycle"));
    }

    #[test]
    fn empty_or_non_type_file_falls_back_to_file_chunk() {
 // package-info.java style - no top-level types.
        let src = "package com.example;\n";
        let chunks = chunk_file(src, "com.example");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, ChunkKind::File);
        assert_eq!(chunks[0].class_fqn, "com.example");
    }

 // -------------------------------------------------------------------
 // Symbol extraction tests
 // -------------------------------------------------------------------

    fn find_class<'a>(out: &'a ChunkerOutput, fqn: &str) -> &'a ClassSymbol {
        out.symbols
            .classes
            .iter()
            .find(|c| c.fqn == fqn)
            .unwrap_or_else(|| {
                panic!(
                    "no ClassSymbol with fqn={fqn}; have {:?}",
                    out.symbols
                        .classes
                        .iter()
                        .map(|c| &c.fqn)
                        .collect::<Vec<_>>()
                )
            })
    }

    fn find_method<'a>(
        out: &'a ChunkerOutput,
        class_fqn: &str,
        name: &str,
    ) -> &'a MethodSymbol {
        out.symbols
            .methods
            .iter()
            .find(|m| m.class_fqn == class_fqn && m.name == name)
            .unwrap_or_else(|| {
                panic!("no MethodSymbol {class_fqn}::{name}")
            })
    }

    #[test]
    fn class_symbol_captures_kind_and_modifiers() {
        let src = r#"
package com.example;
public abstract class Widget implements Renderable, Clickable {
}
"#;
        let out = chunk_and_extract(src, "com.example");
        let w = find_class(&out, "com.example.Widget");
        assert_eq!(w.kind, TypeKind::Class);
        assert_eq!(w.simple_name, "Widget");
        assert!(w.modifiers.iter().any(|m| m == "public"));
        assert!(w.modifiers.iter().any(|m| m == "abstract"));
        assert!(w.superclass.is_none());
        assert_eq!(
            w.interfaces,
            vec!["Renderable".to_string(), "Clickable".to_string()]
        );
    }

    #[test]
    fn class_symbol_captures_superclass() {
        let src = r#"
package com.example;
public class Button extends Widget {}
"#;
        let out = chunk_and_extract(src, "com.example");
        let b = find_class(&out, "com.example.Button");
        assert_eq!(b.superclass.as_deref(), Some("Widget"));
    }

    #[test]
    fn interface_kind_is_interface() {
        let src = r#"
package com.example;
public interface Renderable extends Visible {}
"#;
        let out = chunk_and_extract(src, "com.example");
        let r = find_class(&out, "com.example.Renderable");
        assert_eq!(r.kind, TypeKind::Interface);
        assert!(r.superclass.is_none());
 // For interfaces surface extends-list via `interfaces`.
        assert_eq!(r.interfaces, vec!["Visible".to_string()]);
    }

    #[test]
    fn enum_and_record_kinds() {
        let enum_src = r#"
package com.example;
public enum Color { RED, GREEN, BLUE }
"#;
        let out = chunk_and_extract(enum_src, "com.example");
        assert_eq!(find_class(&out, "com.example.Color").kind, TypeKind::Enum);
 // Enum constants are surfaced as fields.
        let names: Vec<&str> = out.symbols.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"RED"));
        assert!(names.contains(&"GREEN"));
        assert!(names.contains(&"BLUE"));

        let rec_src = r#"
package com.example;
public record Point(int x, int y) {}
"#;
        let out = chunk_and_extract(rec_src, "com.example");
        assert_eq!(
            find_class(&out, "com.example.Point").kind,
            TypeKind::Record
        );
    }

    #[test]
    fn method_symbol_captures_signature() {
        let src = r#"
package com.example;
public class Service {
    public <T> List<T> fetch(String url, int timeoutMs) throws IOException, TimeoutException {
        return null;
    }
}
"#;
        let out = chunk_and_extract(src, "com.example");
        let m = find_method(&out, "com.example.Service", "fetch");
        assert!(m.modifiers.iter().any(|mo| mo == "public"));
        assert!(!m.is_constructor);
        assert_eq!(m.return_type.as_deref(), Some("List<T>"));
        assert_eq!(m.param_types, vec!["String".to_string(), "int".to_string()]);
        assert_eq!(
            m.thrown,
            vec!["IOException".to_string(), "TimeoutException".to_string()]
        );
    }

    #[test]
    fn constructor_symbol_has_no_return_type() {
        let src = r#"
package com.example;
public class PageManager {
    public PageManager(int initialSize) { this.size = initialSize; }
    private int size;
}
"#;
        let out = chunk_and_extract(src, "com.example");
        let c = find_method(&out, "com.example.PageManager", "PageManager");
        assert!(c.is_constructor);
        assert!(c.return_type.is_none());
        assert_eq!(c.param_types, vec!["int".to_string()]);
    }

    #[test]
    fn field_symbols_are_extracted() {
        let src = r#"
package com.example;
public class Box {
    private final int width;
    public static String tag = "box";
    int a, b, c;
}
"#;
        let out = chunk_and_extract(src, "com.example");
        let by_name: std::collections::HashMap<&str, &FieldSymbol> = out
            .symbols
            .fields
            .iter()
            .map(|f| (f.name.as_str(), f))
            .collect();

        let w = by_name.get("width").expect("width field missing");
        assert_eq!(w.class_fqn, "com.example.Box");
        assert_eq!(w.type_text, "int");
        assert!(w.modifiers.iter().any(|m| m == "private"));
        assert!(w.modifiers.iter().any(|m| m == "final"));

        let t = by_name.get("tag").expect("tag field missing");
        assert_eq!(t.type_text, "String");
        assert!(t.modifiers.iter().any(|m| m == "static"));

 // Multi-declarator: `int a, b, c;` emits three records.
        assert!(by_name.contains_key("a"));
        assert!(by_name.contains_key("b"));
        assert!(by_name.contains_key("c"));
    }

    #[test]
    fn varargs_param_preserved_in_signature() {
        let src = r#"
package com.example;
public class Logger {
    public void log(String fmt, Object... args) {}
}
"#;
        let out = chunk_and_extract(src, "com.example");
        let m = find_method(&out, "com.example.Logger", "log");
        assert_eq!(
            m.param_types,
            vec!["String".to_string(), "Object...".to_string()]
        );
    }

    #[test]
    fn nested_class_symbols_carry_outer_fqn() {
        let src = r#"
package com.example;
public class Outer {
    public static class Inner {
        public void tick() {}
        private int counter;
    }
}
"#;
        let out = chunk_and_extract(src, "com.example");
        let _outer = find_class(&out, "com.example.Outer");
        let _inner = find_class(&out, "com.example.Outer.Inner");

        let tick = find_method(&out, "com.example.Outer.Inner", "tick");
        assert!(tick.modifiers.iter().any(|m| m == "public"));

        let counter = out
            .symbols
            .fields
            .iter()
            .find(|f| f.name == "counter")
            .expect("counter field missing");
        assert_eq!(counter.class_fqn, "com.example.Outer.Inner");
    }

    #[test]
    fn annotations_in_modifiers_are_skipped() {
        let src = r#"
package com.example;
public class Api {
    @Override
    @Deprecated
    public String toString() { return "Api"; }
}
"#;
        let out = chunk_and_extract(src, "com.example");
        let m = find_method(&out, "com.example.Api", "toString");
 // Annotations must not leak into the modifier list.
        assert!(m.modifiers.iter().all(|mo| !mo.starts_with('@')));
        assert!(m.modifiers.iter().any(|mo| mo == "public"));
    }

    #[test]
    fn parse_failure_still_returns_chunker_output_with_empty_symbols() {
 // package-info style: no types at all.
        let out = chunk_and_extract("package com.example;\n", "com.example");
        assert_eq!(out.symbols.classes.len(), 0);
        assert_eq!(out.symbols.methods.len(), 0);
        assert_eq!(out.symbols.fields.len(), 0);
 // And a File chunk still gets emitted so the search index isn't
 // missing this file.
        assert_eq!(out.chunks.len(), 1);
        assert_eq!(out.chunks[0].kind, ChunkKind::File);
    }

 /// Opt-in probe: walk a real decompile tree and report chunker stats.
 /// Run with:
 /// ATLAS_PROBE_DECOMPILE=<path> cargo test --lib \
 /// indexer::chunker::tests::probe_real_decompile \
 /// -- --ignored --nocapture
    #[test]
    #[ignore = "reads a large on-disk decompile; opt-in via ATLAS_PROBE_DECOMPILE"]
    fn probe_real_decompile() {
        use std::path::PathBuf;
        let root = match std::env::var("ATLAS_PROBE_DECOMPILE") {
            Ok(v) => PathBuf::from(v),
            Err(_) => {
                eprintln!("set ATLAS_PROBE_DECOMPILE to a decompile root");
                return;
            }
        };
        assert!(root.is_dir(), "not a dir: {}", root.display());

        let mut files = 0u64;
        let mut parse_skipped = 0u64;
        let mut types = 0u64;
        let mut methods = 0u64;
        let mut constructors = 0u64;
        let mut file_fallbacks = 0u64;
        let mut max_chunks_in_file: (u64, String) = (0, String::new());

        for entry in walkdir::WalkDir::new(&root) {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("java") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(path) else {
                parse_skipped += 1;
                continue;
            };
            files += 1;

 // Derive package the same way walker.rs does.
            let rel = path.strip_prefix(&root).unwrap_or(path);
            let package = rel
                .parent()
                .map(|p| {
                    p.components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .unwrap_or_default();

            let chunks = super::chunk_file(&src, &package);
            let n = chunks.len() as u64;
            if n > max_chunks_in_file.0 {
                max_chunks_in_file = (n, rel.display().to_string());
            }
            for c in &chunks {
                match c.kind {
                    ChunkKind::Type => types += 1,
                    ChunkKind::Method => methods += 1,
                    ChunkKind::Constructor => constructors += 1,
                    ChunkKind::File => file_fallbacks += 1,
                }
            }
        }

        eprintln!("\n=== chunker probe ===");
        eprintln!("files indexed     : {files}");
        eprintln!("read errors       : {parse_skipped}");
        eprintln!("Type chunks       : {types}");
        eprintln!("Method chunks     : {methods}");
        eprintln!("Constructor chunks: {constructors}");
        eprintln!("File fallbacks    : {file_fallbacks}");
        eprintln!(
            "fallback ratio    : {:.3}%",
            100.0 * (file_fallbacks as f64) / (files.max(1) as f64)
        );
        eprintln!(
            "avg chunks / file : {:.2}",
            (types + methods + constructors + file_fallbacks) as f64
                / (files.max(1) as f64)
        );
        eprintln!(
            "max chunks in one : {} ({})",
            max_chunks_in_file.0, max_chunks_in_file.1
        );

 // Hard floor: decompiled server source should overwhelmingly parse.
 // If the fallback ratio blows past 5% something is wrong with the
 // chunker or tree-sitter-java, not the input.
        let fallback_ratio = (file_fallbacks as f64) / (files.max(1) as f64);
        assert!(
            fallback_ratio < 0.05,
            "too many File fallbacks: {:.2}% of {} files",
            fallback_ratio * 100.0,
            files
        );
    }

    #[test]
    fn line_ranges_are_one_based_and_inclusive() {
        let src = "package p;\npublic class A {\n  public int x() { return 0; }\n}\n";
        let chunks = chunk_file(src, "p");
        let a = find(&chunks, ChunkKind::Type, "A");
        assert_eq!(a.start_line, 2);
        assert_eq!(a.end_line, 4);
        let x = find(&chunks, ChunkKind::Method, "x");
        assert_eq!(x.start_line, 3);
        assert_eq!(x.end_line, 3);
    }
}
