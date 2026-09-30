use crate::database::Database;
use crate::error::{AppError, Result};
use crate::workspace::Workspace;
use pulldown_cmark::{Event, Options, Parser as MarkdownParser, Tag, TagEnd};
use regex::RegexBuilder;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tree_sitter::{Language, Node, Parser};
use uuid::Uuid;
const MAX_SEARCH_RESULTS: usize = 500;
const MAX_GRAPH_NODES: usize = 20_000;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub content: String,
    pub symbol: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    pub files: usize,
    pub symbols: usize,
    pub dependencies: usize,
    pub duration_ms: u64,
    pub cancelled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
}
#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
}
#[derive(Clone, Debug)]
pub struct Reference {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub column: usize,
}
#[derive(Clone, Debug, Default)]
pub struct ParsedFile {
    pub symbols: Vec<Symbol>,
    pub references: Vec<Reference>,
    pub dependencies: Vec<(String, String)>,
    pub has_errors: bool,
}
pub trait LanguageAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn parse(&self, source: &str) -> Result<ParsedFile>;
}
struct TreeSitterAdapter {
    name: &'static str,
    language: Language,
}
struct MarkdownAdapter;
fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}
fn unquote(value: &str) -> String {
    if value.starts_with('"') {
        if let Ok(decoded) = serde_json::from_str::<String>(value) {
            return decoded;
        }
    }
    value.trim_matches(['\'', '"', '`']).to_owned()
}
fn name_node(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("key"))
}
impl LanguageAdapter for TreeSitterAdapter {
    fn name(&self) -> &str {
        self.name
    }
    fn parse(&self, source: &str) -> Result<ParsedFile> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language)
            .map_err(|error| AppError::new("PARSER_LANGUAGE", error.to_string()))?;
        parser.set_timeout_micros(2_000_000);
        let tree = parser.parse(source, None).ok_or_else(|| {
            AppError::new("PARSER_TIMEOUT", "Parsing exceeded the two second budget")
        })?;
        let mut result = ParsedFile {
            has_errors: tree.root_node().has_error(),
            ..ParsedFile::default()
        };
        let mut stack = vec![tree.root_node()];
        let mut definitions = HashSet::new();
        while let Some(node) = stack.pop() {
            if result.symbols.len() + result.references.len() + result.dependencies.len() > 50_000 {
                return Err(AppError::new(
                    "PARSER_COMPLEXITY",
                    "This file exceeds the 50000 AST item budget",
                ));
            }
            let kind = node.kind();
            let symbol_kind = match kind {
                "function_declaration"
                | "function_definition"
                | "function_item"
                | "generator_function_declaration" => Some("function"),
                "method_definition" | "method_signature" | "function_signature_item" => {
                    Some("method")
                }
                "class_declaration"
                | "class_definition"
                | "abstract_class_declaration"
                | "struct_item" => Some("class"),
                "interface_declaration" | "trait_item" => Some("interface"),
                "type_alias_declaration" | "type_item" | "enum_item" | "enum_declaration" => {
                    Some("type")
                }
                "variable_declarator" | "const_item" | "static_item" => Some("variable"),
                "mod_item" | "internal_module" => Some("module"),
                "pair" if self.name == "JSON" => Some("property"),
                _ => None,
            };
            if let Some(symbol_kind) = symbol_kind {
                if let Some(name) = name_node(node) {
                    let value = unquote(text(name, source));
                    if !value.is_empty() && value.len() <= 512 {
                        definitions.insert(name.start_byte());
                        result.symbols.push(Symbol {
                            name: value,
                            kind: symbol_kind.to_owned(),
                            line: name.start_position().row + 1,
                            column: name.start_position().column + 1,
                            end_line: node.end_position().row + 1,
                        });
                    }
                }
            }
            if kind == "assignment" && self.name == "Python" {
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "identifier" {
                        definitions.insert(left.start_byte());
                        result.symbols.push(Symbol {
                            name: text(left, source).to_owned(),
                            kind: "variable".to_owned(),
                            line: left.start_position().row + 1,
                            column: left.start_position().column + 1,
                            end_line: node.end_position().row + 1,
                        });
                    }
                }
            }
            if kind == "let_declaration" && self.name == "Rust" {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    if pattern.kind() == "identifier" {
                        definitions.insert(pattern.start_byte());
                        result.symbols.push(Symbol {
                            name: text(pattern, source).to_owned(),
                            kind: "variable".to_owned(),
                            line: pattern.start_position().row + 1,
                            column: pattern.start_position().column + 1,
                            end_line: node.end_position().row + 1,
                        });
                    }
                }
            }
            if matches!(kind, "import_statement" | "export_statement") && self.name != "Python" {
                if let Some(module) = node.child_by_field_name("source") {
                    result.dependencies.push((
                        unquote(text(module, source)),
                        if kind == "export_statement" {
                            "exports"
                        } else {
                            "imports"
                        }
                        .to_owned(),
                    ));
                }
            }
            if kind == "import_from_statement" {
                if let Some(module) = node.child_by_field_name("module_name") {
                    result
                        .dependencies
                        .push((text(module, source).to_owned(), "imports".to_owned()));
                }
            }
            if kind == "import_statement" && self.name == "Python" {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    let module = if child.kind() == "aliased_import" {
                        child.child_by_field_name("name").unwrap_or(child)
                    } else {
                        child
                    };
                    result
                        .dependencies
                        .push((text(module, source).to_owned(), "imports".to_owned()));
                }
            }
            if kind == "use_declaration" {
                if let Some(argument) = node.child_by_field_name("argument") {
                    result
                        .dependencies
                        .push((text(argument, source).to_owned(), "imports".to_owned()));
                }
            }
            if kind == "mod_item" && node.child_by_field_name("body").is_none() {
                if let Some(name) = name_node(node) {
                    result
                        .dependencies
                        .push((format!("self::{}", text(name, source)), "module".to_owned()));
                }
            }
            if matches!(kind, "call_expression" | "call") {
                if let Some(function) = node.child_by_field_name("function") {
                    let target = function
                        .child_by_field_name("field")
                        .or_else(|| function.child_by_field_name("property"))
                        .or_else(|| function.child_by_field_name("attribute"))
                        .or_else(|| function.child_by_field_name("name"))
                        .unwrap_or(function);
                    let name = text(target, source);
                    if name.len() <= 512 {
                        result.references.push(Reference {
                            name: name.to_owned(),
                            kind: "calls".to_owned(),
                            line: target.start_position().row + 1,
                            column: target.start_position().column + 1,
                        });
                    }
                    if matches!(name, "require" | "import") {
                        if let Some(args) = node.child_by_field_name("arguments") {
                            if let Some(value) = args.named_child(0) {
                                if matches!(value.kind(), "string" | "string_literal") {
                                    result
                                        .dependencies
                                        .push((unquote(text(value, source)), "imports".to_owned()));
                                }
                            }
                        }
                    }
                }
            }
            if matches!(
                kind,
                "identifier" | "type_identifier" | "property_identifier"
            ) && !definitions.contains(&node.start_byte())
            {
                let value = text(node, source);
                if value.len() <= 512 {
                    result.references.push(Reference {
                        name: value.to_owned(),
                        kind: "references".to_owned(),
                        line: node.start_position().row + 1,
                        column: node.start_position().column + 1,
                    });
                }
            }
            if matches!(
                kind,
                "extends_clause" | "extends_type_clause" | "superclasses"
            ) {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    result.references.push(Reference {
                        name: text(child, source).to_owned(),
                        kind: "inherits".to_owned(),
                        line: child.start_position().row + 1,
                        column: child.start_position().column + 1,
                    });
                }
            }
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        result.dependencies.sort();
        result.dependencies.dedup();
        Ok(result)
    }
}
impl LanguageAdapter for MarkdownAdapter {
    fn name(&self) -> &str {
        "Markdown"
    }
    fn parse(&self, source: &str) -> Result<ParsedFile> {
        let mut result = ParsedFile::default();
        let mut heading: Option<(usize, String)> = None;
        for (event, span) in MarkdownParser::new_ext(source, Options::all()).into_offset_iter() {
            match event {
                Event::Start(Tag::Heading { .. }) => heading = Some((span.start, String::new())),
                Event::Text(value) | Event::Code(value) => {
                    if let Some((_, title)) = heading.as_mut() {
                        title.push_str(&value);
                    }
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((offset, title)) = heading.take() {
                        result.symbols.push(Symbol {
                            name: title,
                            kind: "heading".to_owned(),
                            line: source[..offset]
                                .bytes()
                                .filter(|byte| *byte == b'\n')
                                .count()
                                + 1,
                            column: 1,
                            end_line: source[..span.end]
                                .bytes()
                                .filter(|byte| *byte == b'\n')
                                .count()
                                + 1,
                        });
                    }
                }
                Event::Start(Tag::Link { dest_url, .. }) => result
                    .dependencies
                    .push((dest_url.to_string(), "links".to_owned())),
                _ => {}
            }
        }
        Ok(result)
    }
}
pub fn adapter_for(path: &str) -> Option<Box<dyn LanguageAdapter>> {
    let extension = Path::new(path).extension()?.to_str()?.to_lowercase();
    let (name, language) = match extension.as_str() {
        "ts" | "mts" | "cts" => (
            "TypeScript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        "tsx" => ("TypeScript", tree_sitter_typescript::LANGUAGE_TSX.into()),
        "js" | "jsx" | "mjs" | "cjs" => ("JavaScript", tree_sitter_javascript::LANGUAGE.into()),
        "rs" => ("Rust", tree_sitter_rust::LANGUAGE.into()),
        "py" | "pyi" => ("Python", tree_sitter_python::LANGUAGE.into()),
        "json" => ("JSON", tree_sitter_json::LANGUAGE.into()),
        "md" | "markdown" => return Some(Box::new(MarkdownAdapter)),
        _ => return None,
    };
    Some(Box::new(TreeSitterAdapter { name, language }))
}
struct IndexedFile {
    path: String,
    content: String,
    hash: String,
    language: String,
    parsed: ParsedFile,
    dependencies: Vec<GraphEdge>,
}
pub struct IndexService {
    db: Arc<Database>,
    gate: Mutex<()>,
    search_generation: AtomicU64,
}
impl IndexService {
    pub fn new(db: Arc<Database>) -> Result<Self> {
        db.with(|connection| {
            connection.execute_batch("CREATE TABLE IF NOT EXISTS index_files(repository_id TEXT NOT NULL,path TEXT NOT NULL,content TEXT NOT NULL,hash TEXT NOT NULL,language TEXT NOT NULL,size INTEGER NOT NULL,parse_errors INTEGER NOT NULL,generation TEXT NOT NULL,PRIMARY KEY(repository_id,path));CREATE TABLE IF NOT EXISTS index_symbols(repository_id TEXT NOT NULL,path TEXT NOT NULL,name TEXT NOT NULL,kind TEXT NOT NULL,line INTEGER NOT NULL,column_no INTEGER NOT NULL,end_line INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS index_symbol_name ON index_symbols(repository_id,name COLLATE NOCASE);CREATE INDEX IF NOT EXISTS index_symbol_path ON index_symbols(repository_id,path);CREATE TABLE IF NOT EXISTS index_references(repository_id TEXT NOT NULL,path TEXT NOT NULL,name TEXT NOT NULL,kind TEXT NOT NULL,line INTEGER NOT NULL,column_no INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS index_reference_name ON index_references(repository_id,name);CREATE INDEX IF NOT EXISTS index_reference_path ON index_references(repository_id,path);CREATE TABLE IF NOT EXISTS index_edges(repository_id TEXT NOT NULL,source TEXT NOT NULL,target TEXT NOT NULL,kind TEXT NOT NULL,PRIMARY KEY(repository_id,source,target,kind));CREATE INDEX IF NOT EXISTS index_edge_target ON index_edges(repository_id,target);CREATE TABLE IF NOT EXISTS index_runs(id TEXT PRIMARY KEY,repository_id TEXT NOT NULL,status TEXT NOT NULL,started_at INTEGER NOT NULL,duration_ms INTEGER NOT NULL DEFAULT 0,files INTEGER NOT NULL DEFAULT 0,error TEXT);CREATE INDEX IF NOT EXISTS index_run_repository ON index_runs(repository_id,started_at);UPDATE index_runs SET status='interrupted' WHERE status='running';")?;
            Ok(())
        })?;
        db.with(|connection| {
            let has_text_index: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='index_text')", [], |row| row.get(0))?;
            if !has_text_index {
                let transaction = connection.transaction()?;
                transaction.execute_batch("CREATE VIRTUAL TABLE index_text USING fts5(content,content='index_files',content_rowid='rowid',tokenize='trigram case_sensitive 1');INSERT INTO index_text(index_text) VALUES('rebuild');")?;
                transaction.commit()?;
            }
            Ok(())
        })?;
        Ok(Self {
            db,
            gate: Mutex::new(()),
            search_generation: AtomicU64::new(0),
        })
    }
    pub fn index(&self, workspace: &Workspace, cancel: &AtomicBool) -> Result<IndexStats> {
        let output = crate::git::run(
            workspace,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let paths = output
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| {
                String::from_utf8(part.to_vec()).map_err(|_| {
                    AppError::new("PATH_ENCODING", "Repository contains a non UTF-8 path")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.run(workspace, &paths, &[], true, cancel)
    }
    pub fn update(
        &self,
        workspace: &Workspace,
        paths: &[String],
        cancel: &AtomicBool,
    ) -> Result<IndexStats> {
        let mut scopes = Vec::new();
        for path in paths {
            let path = path.replace('\\', "/");
            if path.split('/').any(|part| {
                matches!(
                    part,
                    ".git" | "node_modules" | "target" | "dist" | ".astraforge"
                )
            }) {
                continue;
            }
            let normalized = crate::workspace::normalize_path(&path)?;
            let scope = if normalized == ".gitignore" {
                String::new()
            } else if normalized.ends_with("/.gitignore") {
                normalized.trim_end_matches("/.gitignore").to_owned()
            } else {
                normalized
            };
            scopes.push(scope);
        }
        scopes.sort();
        scopes.dedup();
        let mut discovered = HashSet::new();
        let mut at = 0;
        while at < scopes.len() {
            if cancel.load(Ordering::Acquire) {
                return Ok(IndexStats {
                    cancelled: true,
                    ..IndexStats::default()
                });
            }
            let mut end = at;
            let mut length = 0;
            while end < scopes.len() && length + scopes[end].len() < 8000 {
                length += scopes[end].len() + 1;
                end += 1;
            }
            let mut args = vec![
                "--literal-pathspecs",
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
            ];
            args.extend(scopes[at..end].iter().map(|scope| {
                if scope.is_empty() {
                    "."
                } else {
                    scope.as_str()
                }
            }));
            let output = crate::git::run(workspace, &args)?;
            for path in output
                .split(|byte| *byte == 0)
                .filter(|part| !part.is_empty())
            {
                discovered.insert(String::from_utf8(path.to_vec()).map_err(|_| {
                    AppError::new("PATH_ENCODING", "Repository contains a non UTF-8 path")
                })?);
            }
            at = end;
        }
        let previous = self.db.with(|connection| {
            let mut statement = connection.prepare("SELECT path FROM index_files WHERE repository_id=?1 AND (?2='' OR path=?2 OR substr(path,1,length(?2)+1)=?2||'/')")?;
            let mut previous = HashSet::new();
            for scope in &scopes {
                for path in statement.query_map(params![workspace.id, scope], |row| row.get::<_, String>(0))? { previous.insert(path?); }
            }
            Ok(previous)
        })?;
        let removed = previous
            .difference(&discovered)
            .cloned()
            .collect::<Vec<_>>();
        let mut paths = discovered.into_iter().collect::<Vec<_>>();
        paths.sort();
        self.run(workspace, &paths, &removed, false, cancel)
    }
    fn run(
        &self,
        workspace: &Workspace,
        paths: &[String],
        removed: &[String],
        full: bool,
        cancel: &AtomicBool,
    ) -> Result<IndexStats> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| AppError::new("INDEX_LOCK", "Indexer lock was poisoned"))?;
        let start = Instant::now();
        let id = Uuid::new_v4().to_string();
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| AppError::new("CLOCK", error.to_string()))?
            .as_millis() as i64;
        self.db.with(|connection| {
            connection.execute("INSERT INTO index_runs(id,repository_id,status,started_at) VALUES(?1,?2,'running',?3)", params![id, workspace.id, started_at])?;
            Ok(())
        })?;
        let outcome = self.run_files(workspace, paths, removed, &id, full, cancel);
        let elapsed = start.elapsed().as_millis() as u64;
        self.db.with(|connection| {
            let (status, files, error) = match &outcome {
                Ok(stats) => (
                    if stats.cancelled {
                        "cancelled"
                    } else {
                        "completed"
                    },
                    stats.files as i64,
                    None,
                ),
                Err(error) => ("failed", 0, Some(error.message.as_str())),
            };
            connection.execute(
                "UPDATE index_runs SET status=?2,files=?3,duration_ms=?4,error=?5 WHERE id=?1",
                params![id, status, files, elapsed as i64, error],
            )?;
            Ok(())
        })?;
        outcome.map(|mut stats| {
            stats.duration_ms = elapsed;
            stats
        })
    }
    fn run_files(
        &self,
        workspace: &Workspace,
        paths: &[String],
        removals: &[String],
        generation: &str,
        full: bool,
        cancel: &AtomicBool,
    ) -> Result<IndexStats> {
        let mut stats = IndexStats::default();
        if cancel.load(Ordering::Acquire) {
            return Ok(IndexStats {
                cancelled: true,
                ..IndexStats::default()
            });
        }
        self.db.with(|connection| {
            let transaction = connection.transaction()?;
            for path in removals {
                transaction.execute("INSERT INTO index_text(index_text,rowid,content) SELECT 'delete',rowid,content FROM index_files WHERE repository_id=?1 AND path=?2", params![workspace.id, path])?;
                for table in ["index_files", "index_symbols", "index_references"] {
                    transaction.execute(&format!("DELETE FROM {table} WHERE repository_id=?1 AND path=?2"), params![workspace.id, path])?;
                }
                transaction.execute("DELETE FROM index_edges WHERE repository_id=?1 AND source=?2", params![workspace.id, path])?;
            }
            transaction.commit()?;
            Ok(())
        })?;
        let mut seen = HashSet::new();
        for batch in paths.chunks(64) {
            if cancel.load(Ordering::Acquire) {
                stats.cancelled = true;
                break;
            }
            let mut files = Vec::new();
            let mut unchanged = Vec::new();
            let mut removed = Vec::new();
            for path in batch {
                if cancel.load(Ordering::Acquire) {
                    stats.cancelled = true;
                    break;
                }
                let path = path.replace('\\', "/");
                if !seen.insert(path.clone())
                    || path.split('/').any(|part| {
                        matches!(
                            part,
                            ".git" | "node_modules" | "target" | "dist" | ".astraforge"
                        )
                    })
                {
                    continue;
                }
                let file = match workspace.read(&path) {
                    Ok(file) => file,
                    Err(error)
                        if matches!(
                            error.code.as_str(),
                            "NOT_FOUND"
                                | "NOT_TEXT_FILE"
                                | "FILE_TOO_LARGE"
                                | "BINARY_FILE"
                                | "UNSUPPORTED_ENCODING"
                                | "PATH_SYMLINK"
                                | "PATH_REPARSE"
                        ) =>
                    {
                        removed.push(path);
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let content = file.content;
                let hash = file.hash;
                let previous = self.db.with(|connection| {
                    Ok(connection
                        .query_row(
                            "SELECT hash FROM index_files WHERE repository_id=?1 AND path=?2",
                            params![workspace.id, path],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?)
                })?;
                if previous.as_deref() == Some(&hash) {
                    unchanged.push(path);
                    continue;
                }
                let (language, parsed) = match adapter_for(&path) {
                    Some(adapter) => (adapter.name().to_owned(), adapter.parse(&content)?),
                    None => ("Text".to_owned(), ParsedFile::default()),
                };
                let dependencies = parsed
                    .dependencies
                    .iter()
                    .map(|(target, kind)| GraphEdge {
                        source: path.clone(),
                        target: resolve_dependency(workspace, &path, target, &language),
                        kind: kind.clone(),
                    })
                    .collect();
                stats.symbols += parsed.symbols.len();
                stats.dependencies += parsed.dependencies.len();
                stats.files += 1;
                files.push(IndexedFile {
                    path,
                    content,
                    hash,
                    language,
                    parsed,
                    dependencies,
                });
            }
            self.db.with(|connection| {
                let transaction = connection.transaction()?;
                for path in unchanged {
                    transaction.execute("UPDATE index_files SET generation=?3 WHERE repository_id=?1 AND path=?2", params![workspace.id, path, generation])?;
                }
                for path in removed.iter().chain(files.iter().map(|file| &file.path)) {
                    transaction.execute("INSERT INTO index_text(index_text,rowid,content) SELECT 'delete',rowid,content FROM index_files WHERE repository_id=?1 AND path=?2", params![workspace.id, path])?;
                    for table in ["index_files", "index_symbols", "index_references"] {
                        transaction.execute(&format!("DELETE FROM {table} WHERE repository_id=?1 AND path=?2"), params![workspace.id, path])?;
                    }
                    transaction.execute("DELETE FROM index_edges WHERE repository_id=?1 AND source=?2", params![workspace.id, path])?;
                }
                for file in files {
                    transaction.execute("INSERT INTO index_files(repository_id,path,content,hash,language,size,parse_errors,generation) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![workspace.id, file.path, file.content, file.hash, file.language, file.content.len() as i64, file.parsed.has_errors, generation])?;
                    transaction.execute("INSERT INTO index_text(rowid,content) VALUES(?1,?2)", params![transaction.last_insert_rowid(), file.content])?;
                    let mut symbols = transaction.prepare_cached("INSERT INTO index_symbols(repository_id,path,name,kind,line,column_no,end_line) VALUES(?1,?2,?3,?4,?5,?6,?7)")?;
                    for symbol in file.parsed.symbols {
                        symbols.execute(params![workspace.id, file.path, symbol.name, symbol.kind, symbol.line as i64, symbol.column as i64, symbol.end_line as i64])?;
                    }
                    let mut references = transaction.prepare_cached("INSERT INTO index_references(repository_id,path,name,kind,line,column_no) VALUES(?1,?2,?3,?4,?5,?6)")?;
                    for reference in file.parsed.references {
                        references.execute(params![workspace.id, file.path, reference.name, reference.kind, reference.line as i64, reference.column as i64])?;
                    }
                    let mut edges = transaction.prepare_cached("INSERT OR IGNORE INTO index_edges(repository_id,source,target,kind) VALUES(?1,?2,?3,?4)")?;
                    for edge in file.dependencies {
                        edges.execute(params![workspace.id, edge.source, edge.target, edge.kind])?;
                    }
                }
                transaction.commit()?;
                Ok(())
            })?;
        }
        if full && !stats.cancelled {
            self.db.with(|connection| {
                let transaction = connection.transaction()?;
                transaction.execute("INSERT INTO index_text(index_text,rowid,content) SELECT 'delete',rowid,content FROM index_files WHERE repository_id=?1 AND generation<>?2", params![workspace.id, generation])?;
                transaction.execute("DELETE FROM index_files WHERE repository_id=?1 AND generation<>?2", params![workspace.id, generation])?;
                for table in ["index_symbols", "index_references"] {
                    transaction.execute(&format!("DELETE FROM {table} WHERE repository_id=?1 AND path NOT IN(SELECT path FROM index_files WHERE repository_id=?1)"), [&workspace.id])?;
                }
                transaction.execute("DELETE FROM index_edges WHERE repository_id=?1 AND source NOT IN(SELECT path FROM index_files WHERE repository_id=?1)", [&workspace.id])?;
                transaction.commit()?;
                Ok(())
            })?;
        }
        Ok(stats)
    }
    pub fn search(
        &self,
        workspace: &Workspace,
        query: &str,
        mode: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let generation = self.search_generation.load(Ordering::Acquire);
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if query.len() > 4096 || offset > 100_000 {
            return Err(AppError::new(
                "SEARCH_LIMIT",
                "Search query or page offset exceeds the supported limit",
            ));
        }
        let limit = limit.min(MAX_SEARCH_RESULTS);
        self.db.with(|connection| {
            if mode == "symbol" {
                let mut statement = connection.prepare("SELECT s.path,s.line,s.column_no,s.name,s.kind FROM index_symbols s WHERE s.repository_id=?1 AND instr(lower(s.name),lower(?2))>0 ORDER BY s.name,s.path,s.line LIMIT ?3 OFFSET ?4")?;
                let hits = statement.query_map(params![workspace.id, query, limit as i64, offset as i64], |row| Ok(SearchHit { path: row.get(0)?, line: row.get::<_, i64>(1)? as usize, column: row.get::<_, i64>(2)? as usize, content: format!("{} {}", row.get::<_, String>(4)?, row.get::<_, String>(3)?), symbol: Some(row.get(3)?) }))?.collect::<std::result::Result<Vec<_>, _>>()?;
                return Ok(hits);
            }
            if mode == "filename" {
                let mut statement = connection.prepare("SELECT path FROM index_files WHERE repository_id=?1 AND instr(lower(path),lower(?2))>0 ORDER BY path LIMIT ?3 OFFSET ?4")?;
                let hits = statement.query_map(params![workspace.id, query, limit as i64, offset as i64], |row| { let path: String = row.get(0)?; Ok(SearchHit { content: path.clone(), path, line: 1, column: 1, symbol: None }) })?.collect::<std::result::Result<Vec<_>, _>>()?;
                return Ok(hits);
            }
            let regex = match mode {
                "text" => None,
                "regex" => Some(RegexBuilder::new(query).size_limit(2 * 1024 * 1024).dfa_size_limit(2 * 1024 * 1024).build().map_err(|error| AppError::new("INVALID_REGEX", error.to_string()))?),
                _ => return Err(AppError::new("SEARCH_MODE", "Unknown search mode")),
            };
            let trigram = regex.is_none() && query.chars().count() >= 3;
            let sql = if trigram { "SELECT f.path,f.content FROM index_text CROSS JOIN index_files f ON f.rowid=index_text.rowid WHERE index_text MATCH ?2 AND f.repository_id=?1 ORDER BY f.path" } else { "SELECT path,content FROM index_files WHERE repository_id=?1 AND ?2 IS NOT NULL ORDER BY path" };
            let search_term = if trigram { format!("\"{}\"", query.replace('"', "\"\"")) } else { query.to_owned() };
            let mut statement = connection.prepare(sql)?;
            let mut rows = statement.query(params![workspace.id, search_term])?;
            let mut hits = Vec::new();
            let mut skipped = 0;
            let mut scanned_bytes = 0_usize;
            let started = Instant::now();
            while let Some(row) = rows.next()? {
                if self.search_generation.load(Ordering::Acquire) != generation { return Err(AppError::new("SEARCH_CANCELLED", "Search was cancelled")); }
                let path: String = row.get(0)?;
                let content: String = row.get(1)?;
                scanned_bytes += content.len();
                if scanned_bytes > 128 * 1024 * 1024 || started.elapsed().as_secs() >= 5 { return Err(AppError::new("SEARCH_BUDGET", "Search exceeded 128 MiB or five seconds; narrow the query")); }
                for (line_index, line) in content.lines().enumerate() {
                    if line_index % 256 == 0 && self.search_generation.load(Ordering::Acquire) != generation { return Err(AppError::new("SEARCH_CANCELLED", "Search was cancelled")); }
                    let match_at = match &regex { Some(regex) => regex.find(line).map(|found| found.start()), None => line.find(query) };
                    if let Some(column) = match_at {
                        if skipped < offset { skipped += 1; continue; }
                        let symbol = connection.query_row("SELECT name FROM index_symbols WHERE repository_id=?1 AND path=?2 AND line<=?3 AND end_line>=?3 ORDER BY line DESC LIMIT 1", params![workspace.id, path, (line_index + 1) as i64], |row| row.get(0)).optional()?;
                        hits.push(SearchHit { path: path.clone(), line: line_index + 1, column: line[..column].chars().count() + 1, content: line.chars().take(1000).collect(), symbol });
                        if hits.len() >= limit { return Ok(hits); }
                    }
                }
            }
            Ok(hits)
        })
    }
    pub fn cancel_search(&self) {
        self.search_generation.fetch_add(1, Ordering::AcqRel);
    }
    pub fn graph(
        &self,
        workspace: &Workspace,
        path: &str,
        direction: &str,
    ) -> Result<Vec<GraphEdge>> {
        self.db.with(|connection| {
            if matches!(direction, "references" | "callers" | "definition") {
                let query = path.rsplit('#').next().unwrap_or(path).split(':').next().unwrap_or(path);
                if direction == "definition" {
                    let mut statement = connection.prepare("SELECT path,line,name FROM index_symbols WHERE repository_id=?1 AND name=?2 ORDER BY path,line LIMIT 500")?;
                    let results = statement.query_map(params![workspace.id, query], |row| Ok(GraphEdge { source: query.to_owned(), target: format!("{}:{}", row.get::<_, String>(0)?, row.get::<_, i64>(1)?), kind: "definition".to_owned() }))?.collect::<std::result::Result<Vec<_>, _>>()?;
                    return Ok(results);
                }
                let mut statement = connection.prepare("SELECT path,line,kind FROM index_references WHERE repository_id=?1 AND name=?2 AND (?3='references' OR kind='calls') ORDER BY path,line LIMIT 500")?;
                let results = statement.query_map(params![workspace.id, query, direction], |row| Ok(GraphEdge { source: format!("{}:{}", row.get::<_, String>(0)?, row.get::<_, i64>(1)?), target: query.to_owned(), kind: row.get(2)? }))?.collect::<std::result::Result<Vec<_>, _>>()?;
                return Ok(results);
            }
            if !matches!(direction, "dependencies" | "dependents" | "outgoing" | "incoming" | "affected" | "depth") {
                return Err(AppError::new("GRAPH_DIRECTION", "Unknown graph query direction"));
            }
            let reverse = matches!(direction, "dependents" | "incoming" | "affected");
            let recursive = matches!(direction, "affected" | "depth");
            let sql = if reverse { "SELECT source,target,kind FROM index_edges WHERE repository_id=?1 AND target=?2 ORDER BY source" } else { "SELECT source,target,kind FROM index_edges WHERE repository_id=?1 AND source=?2 ORDER BY target" };
            let mut statement = connection.prepare(sql)?;
            let mut queue = VecDeque::from([path.to_owned()]);
            let mut visited = HashSet::new();
            let mut unique = HashSet::new();
            let mut results = Vec::new();
            while let Some(current) = queue.pop_front() {
                if !visited.insert(current.clone()) { continue; }
                if visited.len() > MAX_GRAPH_NODES { return Err(AppError::new("GRAPH_LIMIT", "Graph traversal exceeded 20000 nodes")); }
                let edges = statement.query_map(params![workspace.id, current], |row| Ok(GraphEdge { source: row.get(0)?, target: row.get(1)?, kind: row.get(2)? }))?;
                for edge in edges {
                    let edge = edge?;
                    if recursive { queue.push_back(if reverse { edge.source.clone() } else { edge.target.clone() }); }
                    if unique.insert(edge.clone()) { results.push(edge); }
                    if results.len() > 100_000 { return Err(AppError::new("GRAPH_LIMIT", "Graph traversal exceeded 100000 edges")); }
                }
            }
            Ok(results)
        })
    }
    pub fn health(&self, workspace: &Workspace) -> Result<Value> {
        self.db.with(|connection| {
            let mut languages = BTreeMap::new();
            let mut statement = connection.prepare("SELECT language,count(*) FROM index_files WHERE repository_id=?1 GROUP BY language")?;
            for row in statement.query_map([&workspace.id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))? {
                let (name, count) = row?;
                languages.insert(name, count);
            }
            let file_count: i64 = connection.query_row("SELECT count(*) FROM index_files WHERE repository_id=?1", [&workspace.id], |row| row.get(0))?;
            let dependency_count: i64 = connection.query_row("SELECT count(*) FROM index_edges WHERE repository_id=?1", [&workspace.id], |row| row.get(0))?;
            let symbol_count: i64 = connection.query_row("SELECT count(*) FROM index_symbols WHERE repository_id=?1", [&workspace.id], |row| row.get(0))?;
            let reference_count: i64 = connection.query_row("SELECT count(*) FROM index_references WHERE repository_id=?1", [&workspace.id], |row| row.get(0))?;
            let parse_error_files: i64 = connection.query_row("SELECT count(*) FROM index_files WHERE repository_id=?1 AND parse_errors=1", [&workspace.id], |row| row.get(0))?;
            let test_count: i64 = connection.query_row("SELECT count(*) FROM index_files WHERE repository_id=?1 AND (path LIKE '%.test.%' OR path LIKE '%.spec.%' OR path LIKE '%/test_%' OR path LIKE 'test_%' OR path LIKE '%/tests/%')", [&workspace.id], |row| row.get(0))?;
            let mut largest = connection.prepare("SELECT path,size FROM index_files WHERE repository_id=?1 ORDER BY size DESC LIMIT 10")?;
            let largest = largest.query_map([&workspace.id], |row| Ok(json!({"path":row.get::<_, String>(0)?,"size":row.get::<_, i64>(1)?})))?.collect::<std::result::Result<Vec<_>, _>>()?;
            let mut config = connection.prepare("SELECT path FROM index_files WHERE repository_id=?1 AND (path IN('package.json','Cargo.toml','pyproject.toml','tsconfig.json','requirements.txt','go.mod','Makefile') OR path LIKE '%/package.json') ORDER BY path LIMIT 100")?;
            let config = config.query_map([&workspace.id], |row| row.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
            let last_run = connection.query_row("SELECT status,started_at,duration_ms,files,error FROM index_runs WHERE repository_id=?1 ORDER BY started_at DESC LIMIT 1", [&workspace.id], |row| Ok(json!({"status":row.get::<_, String>(0)?,"startedAt":row.get::<_, i64>(1)?,"durationMs":row.get::<_, i64>(2)?,"files":row.get::<_, i64>(3)?,"error":row.get::<_, Option<String>>(4)?}))).optional()?;
            Ok(json!({"fileCount":file_count,"symbolCount":symbol_count,"referenceCount":reference_count,"dependencyCount":dependency_count,"testCount":test_count,"testCountMethod":"Filename conventions","languageDistribution":languages,"largestFiles":largest,"configuration":config,"parseErrorFiles":parse_error_files,"indexing":last_run,"referenceResolution":"AST name references; compiler-level type resolution is not available"}))
        })
    }
    pub fn documents(
        &self,
        workspace: &Workspace,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<(String, String, String)>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare("SELECT path,content,hash FROM index_files WHERE repository_id=?1 ORDER BY path LIMIT ?2 OFFSET ?3")?;
            let rows = statement.query_map(params![workspace.id, limit.min(100) as i64, offset as i64], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }
}
fn resolve_dependency(workspace: &Workspace, source: &str, target: &str, language: &str) -> String {
    let target = target.split('#').next().unwrap_or(target);
    let parent = Path::new(source).parent().unwrap_or(Path::new(""));
    let mut bases = Vec::new();
    if target.starts_with('.') || language == "Markdown" {
        if language == "Python" {
            let dots = target
                .chars()
                .take_while(|character| *character == '.')
                .count();
            let mut base = parent.to_path_buf();
            for _ in 1..dots {
                base.pop();
            }
            bases.push(base.join(target[dots..].replace('.', "/")));
        } else {
            bases.push(parent.join(target));
        }
    } else if language == "Python" {
        bases.push(Path::new(&target.replace('.', "/")).to_path_buf());
        bases.push(parent.join(target.replace('.', "/")));
    } else if language == "Rust" {
        let module = target
            .split("::{")
            .next()
            .unwrap_or(target)
            .trim_end_matches("::*");
        let stem = Path::new(source)
            .file_stem()
            .and_then(|part| part.to_str())
            .unwrap_or("");
        let module_base = if matches!(stem, "lib" | "main" | "mod") {
            parent.to_path_buf()
        } else {
            parent.join(stem)
        };
        if let Some(rest) = module.strip_prefix("crate::") {
            bases.push(Path::new("src").join(rest.replace("::", "/")));
        }
        if let Some(rest) = module.strip_prefix("self::") {
            bases.push(module_base.join(rest.replace("::", "/")));
        }
        if let Some(rest) = module.strip_prefix("super::") {
            bases.push(
                module_base
                    .parent()
                    .unwrap_or(parent)
                    .join(rest.replace("::", "/")),
            );
        }
        let original = bases.clone();
        for mut base in original {
            while base.pop() && base.components().count() > 1 {
                bases.push(base.clone());
            }
        }
    }
    for base in bases {
        let mut normalized = std::path::PathBuf::new();
        let mut valid = true;
        for component in base.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !normalized.pop() {
                        valid = false;
                        break;
                    }
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if !valid {
            continue;
        }
        let stem = normalized.to_string_lossy().replace('\\', "/");
        let mut candidates = vec![stem.clone()];
        if matches!(language, "TypeScript" | "JavaScript") && stem.ends_with(".js") {
            candidates.push(format!("{}.ts", stem.trim_end_matches(".js")));
            candidates.push(format!("{}.tsx", stem.trim_end_matches(".js")));
        }
        for suffix in [
            ".ts",
            ".tsx",
            ".js",
            ".jsx",
            ".mts",
            ".mjs",
            ".json",
            ".py",
            ".rs",
            "/index.ts",
            "/index.tsx",
            "/index.js",
            "/__init__.py",
            "/mod.rs",
        ] {
            candidates.push(format!("{stem}{suffix}"));
        }
        for candidate in candidates {
            if let Ok(absolute) = workspace.resolve(&candidate) {
                if absolute.is_file() {
                    return candidate;
                }
            }
        }
    }
    format!("module:{target}")
}
