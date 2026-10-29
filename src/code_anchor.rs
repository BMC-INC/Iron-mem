//! AST-bound memory anchors for code drift resistance.
//!
//! v1 is intentionally concrete: Rust files are parsed with tree-sitter and
//! memories that mention a symbol are anchored to that symbol's AST hash. When a
//! file moves or a function is refactored into another path without changing its
//! AST body, `relink_project` updates anchor paths so memory follows the code.

use anyhow::Result;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

use crate::db::{self, CodeAnchor, Database};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustSymbol {
    pub path: String,
    pub kind: String,
    pub name: String,
    pub ast_hash: String,
    pub context_hash: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CodeRelinkReport {
    pub scanned_symbols: usize,
    pub anchors_created: usize,
    pub anchors_relinked: usize,
    pub dry_run: bool,
}

fn hash_text(input: &str) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut h = Sha256::new();
    h.update(normalized.as_bytes());
    format!("{:x}", h.finalize())
}

fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

fn field_text(node: Node, source: &str, field: &str) -> Option<String> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn symbol_name(node: Node, source: &str) -> Option<String> {
    field_text(node, source, "name").or_else(|| field_text(node, source, "type"))
}

fn is_symbol_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_item" | "struct_item" | "enum_item" | "trait_item" | "impl_item" | "mod_item"
    )
}

fn friendly_kind(kind: &str) -> &'static str {
    match kind {
        "function_item" => "function",
        "struct_item" => "struct",
        "enum_item" => "enum",
        "trait_item" => "trait",
        "impl_item" => "impl",
        "mod_item" => "module",
        _ => "symbol",
    }
}

fn collect_symbols(node: Node, source: &str, rel_path: &str, out: &mut Vec<RustSymbol>) {
    if is_symbol_kind(node.kind()) {
        if let Some(name) = symbol_name(node, source) {
            let text = node_text(node, source);
            let parent_text = node.parent().map(|p| node_text(p, source)).unwrap_or(text);
            out.push(RustSymbol {
                path: rel_path.to_string(),
                kind: friendly_kind(node.kind()).to_string(),
                name,
                ast_hash: hash_text(text),
                context_hash: hash_text(parent_text),
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_symbols(child, source, rel_path, out);
    }
}

pub fn parse_rust_symbols(
    path: &Path,
    source: &str,
    project_root: &Path,
) -> Result<Vec<RustSymbol>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;
    let rel_path = path
        .strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let mut symbols = Vec::new();
    collect_symbols(tree.root_node(), source, &rel_path, &mut symbols);
    Ok(symbols)
}

const MAX_SCAN_DEPTH: usize = 64;
const MAX_SCAN_ENTRIES: usize = 100_000;
const MAX_RUST_FILES: usize = 10_000;
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: u64 = 128 * 1024 * 1024;

fn rust_files(
    directory: &Path,
    project_root: &Path,
    depth: usize,
    entries: &mut usize,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    anyhow::ensure!(
        depth <= MAX_SCAN_DEPTH,
        "code scan exceeds maximum directory depth"
    );
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        *entries += 1;
        anyhow::ensure!(
            *entries <= MAX_SCAN_ENTRIES,
            "code scan exceeds entry limit"
        );
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        // Never follow links: they can escape the project or create cycles.
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let canonical = path.canonicalize()?;
        anyhow::ensure!(
            canonical.starts_with(project_root),
            "code scan escaped project root"
        );
        if kind.is_dir() {
            rust_files(&canonical, project_root, depth + 1, entries, out)?;
        } else if kind.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
            anyhow::ensure!(
                out.len() < MAX_RUST_FILES,
                "code scan exceeds Rust file limit"
            );
            out.push(canonical);
        }
    }
    Ok(())
}

pub fn scan_project(project: &str) -> Result<Vec<RustSymbol>> {
    let root = Path::new(project).canonicalize()?;
    anyhow::ensure!(root.is_dir(), "code scan project must be a directory");
    let mut files = Vec::new();
    rust_files(&root, &root, 0, &mut 0, &mut files)?;
    let mut out = Vec::new();
    let mut total_bytes = 0;
    for path in files {
        // Recheck before opening, then bound the read even if a file grows.
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.is_file(),
            "code scan source must be a regular file"
        );
        let path = path.canonicalize()?;
        anyhow::ensure!(path.starts_with(&root), "code scan escaped project root");
        let file = std::fs::File::open(&path)?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file(),
            "code scan source must be a regular file"
        );
        anyhow::ensure!(
            metadata.len() <= MAX_SOURCE_BYTES,
            "code scan source exceeds file size limit"
        );
        let mut source = String::new();
        file.take(MAX_SOURCE_BYTES + 1)
            .read_to_string(&mut source)?;
        anyhow::ensure!(
            source.len() as u64 <= MAX_SOURCE_BYTES,
            "code scan source exceeds file size limit"
        );
        total_bytes += source.len() as u64;
        anyhow::ensure!(
            total_bytes <= MAX_TOTAL_SOURCE_BYTES,
            "code scan exceeds total source size limit"
        );
        out.extend(parse_rust_symbols(&path, &source, &root)?);
    }
    Ok(out)
}

fn memory_mentions_symbol(summary: &str, tags: Option<&str>, symbol: &RustSymbol) -> bool {
    let haystack = format!(
        "{} {}",
        summary.to_ascii_lowercase(),
        tags.unwrap_or("").to_ascii_lowercase()
    );
    let name = symbol.name.to_ascii_lowercase();
    haystack.contains(&name) || haystack.contains(&symbol.path.to_ascii_lowercase())
}

pub async fn anchor_project(
    db: &Database,
    project: &str,
    dry_run: bool,
) -> Result<CodeRelinkReport> {
    let symbols = scan_project(project)?;
    let memories = db::all_memory_ids_with_text(db, Some(project)).await?;
    let mut report = CodeRelinkReport {
        scanned_symbols: symbols.len(),
        dry_run,
        ..Default::default()
    };

    for (memory_id, summary, tags) in memories {
        for symbol in symbols
            .iter()
            .filter(|s| memory_mentions_symbol(&summary, tags.as_deref(), s))
        {
            report.anchors_created += 1;
            if !dry_run {
                db::upsert_code_anchor(
                    db,
                    project,
                    memory_id,
                    &symbol.path,
                    "rust",
                    &symbol.kind,
                    &symbol.name,
                    &symbol.ast_hash,
                    &symbol.context_hash,
                    symbol.start_byte as i64,
                    symbol.end_byte as i64,
                )
                .await?;
            }
        }
    }
    Ok(report)
}

pub async fn relink_project(
    db: &Database,
    project: &str,
    dry_run: bool,
) -> Result<CodeRelinkReport> {
    let symbols = scan_project(project)?;
    let anchors = db::code_anchors_for_project(db, project).await?;
    let mut report = CodeRelinkReport {
        scanned_symbols: symbols.len(),
        dry_run,
        ..Default::default()
    };

    for anchor in anchors {
        if let Some(new_symbol) = matching_symbol(&anchor, &symbols) {
            if new_symbol.path != anchor.path
                || new_symbol.start_byte as i64 != anchor.start_byte
                || new_symbol.end_byte as i64 != anchor.end_byte
            {
                report.anchors_relinked += 1;
                if !dry_run {
                    db::update_code_anchor_location(
                        db,
                        anchor.id,
                        &new_symbol.path,
                        new_symbol.start_byte as i64,
                        new_symbol.end_byte as i64,
                        &new_symbol.context_hash,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(report)
}

fn matching_symbol<'a>(anchor: &CodeAnchor, symbols: &'a [RustSymbol]) -> Option<&'a RustSymbol> {
    symbols
        .iter()
        .find(|s| {
            s.name == anchor.symbol_name
                && s.ast_hash == anchor.ast_hash
                && s.kind == anchor.symbol_kind
        })
        .or_else(|| {
            symbols
                .iter()
                .find(|s| s.name == anchor.symbol_name && s.ast_hash == anchor.ast_hash)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_rejects_excessive_depth_and_oversized_sources() {
        let dir = tempfile::tempdir().unwrap();
        let mut nested = dir.path().to_path_buf();
        for _ in 0..=MAX_SCAN_DEPTH {
            nested = nested.join("d");
            std::fs::create_dir(&nested).unwrap();
        }
        assert!(scan_project(dir.path().to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("directory depth"));

        let large = tempfile::tempdir().unwrap();
        std::fs::File::create(large.path().join("large.rs"))
            .unwrap()
            .set_len(MAX_SOURCE_BYTES + 1)
            .unwrap();
        assert!(scan_project(large.path().to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("file size limit"));
    }

    #[test]
    fn scan_bounds_entries_and_file_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("valid.rs"), "fn inside() {}\n").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut entries = MAX_SCAN_ENTRIES;
        assert!(rust_files(&root, &root, 0, &mut entries, &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("entry limit"));
        let mut files = vec![root.join("existing.rs"); MAX_RUST_FILES];
        assert!(rust_files(&root, &root, 0, &mut 0, &mut files)
            .unwrap_err()
            .to_string()
            .contains("Rust file limit"));
    }

    #[cfg(unix)]
    #[test]
    fn scan_skips_external_links_and_directory_cycles() {
        use std::os::unix::fs::symlink;
        let project = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("inside.rs"), "fn inside() {}\n").unwrap();
        std::fs::write(
            external.path().join("outside.rs"),
            "fn secret_outside() {}\n",
        )
        .unwrap();
        symlink(external.path(), project.path().join("external")).unwrap();
        symlink(
            external.path().join("outside.rs"),
            project.path().join("linked.rs"),
        )
        .unwrap();
        symlink(project.path(), project.path().join("cycle")).unwrap();
        let symbols = scan_project(project.path().to_str().unwrap()).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "inside");
        assert_eq!(symbols[0].path, "inside.rs");
    }

    #[test]
    fn parses_rust_function_and_hashes_ast() {
        let tmp = std::env::temp_dir();
        let file = tmp.join("ironmem_anchor_test.rs");
        let source = "fn target_name() { let x = 1; }\nstruct Holder { value: i32 }\n";
        let syms = parse_rust_symbols(&file, source, &tmp).unwrap();
        assert!(syms
            .iter()
            .any(|s| s.kind == "function" && s.name == "target_name"));
        assert!(syms
            .iter()
            .any(|s| s.kind == "struct" && s.name == "Holder"));
        assert!(syms.iter().all(|s| s.ast_hash.len() == 64));
    }
}
