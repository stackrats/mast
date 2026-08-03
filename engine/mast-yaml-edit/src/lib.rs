//! Lossless span-based YAML editing (plan §6, proven feasible in ADR-0003).
//!
//! A closed edit vocabulary over a tree-sitter-yaml CST: every edit is a
//! string splice into the original buffer, so bytes outside the edit span are
//! untouched by construction — comments, quoting, indentation, CRLF, weird
//! keys all survive. Every [`apply`] is self-verifying: the result must
//! re-parse cleanly (tree-sitter AND saphyr) and the semantic delta must be
//! exactly the intended one (whole-document equality is unsound under
//! anchors/merge keys — the delta walk catches alias ripples and refuses).
//!
//! Design facts binding on this module come from ADR-0003:
//! - splice the concrete scalar node, never its block_node/flow_node wrapper
//!   (wrappers carry anchors/tags);
//! - trailing comments are siblings — insertion must be line-aware;
//! - newline style is detected from the buffer;
//! - anchored-region edits are refused by the delta gate, not by guesswork.

mod verify;

use saphyr::{LoadableYamlNode, Yaml};
use tree_sitter::{Node, Tree};

#[derive(Debug, thiserror::Error)]
pub enum YamlEditError {
    #[error("YAML parse error: {0}")]
    Parse(String),
    #[error("path not found: {0}")]
    PathNotFound(String),
    #[error("unsupported edit: {0}")]
    Unsupported(String),
    #[error("duplicate key: {0}")]
    DuplicateKey(String),
    #[error("edit refused — semantic verification failed: {0}")]
    VerifyFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

pub fn key(s: &str) -> PathSeg {
    PathSeg::Key(s.to_string())
}

pub fn index(i: usize) -> PathSeg {
    PathSeg::Index(i)
}

fn path_display(path: &[PathSeg]) -> String {
    path.iter()
        .map(|seg| match seg {
            PathSeg::Key(k) => k.clone(),
            PathSeg::Index(i) => format!("[{i}]"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The closed edit vocabulary. `value` fields are raw YAML scalar/flow text,
/// spliced verbatim (callers quote as needed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Replace the scalar value at `path`.
    SetScalar { path: Vec<PathSeg>, value: String },
    /// Append `key: value` to the block mapping at `path`.
    InsertMapKey { path: Vec<PathSeg>, key: String, value: String },
    /// Append `key:` with a nested block body to the block mapping at `path`.
    /// `lines` carry their indentation relative to the block's own level
    /// (e.g. `["ports:", "  - '80:80'"]`); apply re-indents to the file.
    InsertMapBlock { path: Vec<PathSeg>, key: String, lines: Vec<String> },
    /// Append `- value` to the block sequence at `path`.
    InsertSeqItem { path: Vec<PathSeg>, value: String },
    /// Remove the mapping entry whose key is the last segment of `path`.
    RemoveKey { path: Vec<PathSeg> },
    /// Rename the mapping key at `path` to `to`, leaving its value untouched.
    /// Only the key text is rewritten, so the entry keeps its position and the
    /// value's formatting is byte-identical.
    RenameKey { path: Vec<PathSeg>, to: String },
    /// Remove the sequence item at the trailing `Index` segment of `path`.
    RemoveSeqItem { path: Vec<PathSeg> },
}

#[derive(Debug, Clone)]
pub struct AppliedEdit {
    pub new_source: String,
    /// Replaced byte range in the OLD source.
    pub old_range: (usize, usize),
    pub inserted_len: usize,
}

// ---------- CST plumbing ----------

fn parse(source: &str) -> Result<Tree, YamlEditError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .map_err(|e| YamlEditError::Parse(e.to_string()))?;
    let tree = parser.parse(source, None).ok_or(YamlEditError::Parse("parser failed".into()))?;
    Ok(tree)
}

fn has_errors(tree: &Tree) -> bool {
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            return true;
        }
        for i in 0..node.child_count() {
            stack.push(node.child(i as u32).unwrap());
        }
    }
    false
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    &source[node.byte_range()]
}

/// Descend through stream/document/block_node/flow_node wrappers (skipping
/// anchors/tags/comments/directives) to the concrete collection or scalar.
fn unwrap_node(mut node: Node) -> Node {
    loop {
        match node.kind() {
            "stream" | "document" | "block_node" | "flow_node" => {
                let mut next = None;
                for i in 0..node.named_child_count() {
                    let child = node.named_child(i as u32).unwrap();
                    match child.kind() {
                        "anchor" | "tag" | "comment" | "yaml_directive" | "tag_directive"
                        | "reserved_directive" => continue,
                        _ => {
                            next = Some(child);
                            break;
                        }
                    }
                }
                match next {
                    Some(child) => node = child,
                    None => return node,
                }
            }
            _ => return node,
        }
    }
}

fn unescape_double_quoted(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn key_text(key_node: Node, source: &str) -> String {
    let node = unwrap_node(key_node);
    let raw = text(node, source);
    match node.kind() {
        "single_quote_scalar" => raw[1..raw.len() - 1].replace("''", "'"),
        "double_quote_scalar" => unescape_double_quoted(&raw[1..raw.len() - 1]),
        _ => raw.trim_end().to_string(),
    }
}

fn find_pair<'t>(map: Node<'t>, source: &str, wanted: &str) -> Option<Node<'t>> {
    let pair_kind = if map.kind() == "flow_mapping" { "flow_pair" } else { "block_mapping_pair" };
    let mut cursor = map.walk();
    let pairs: Vec<Node> =
        map.named_children(&mut cursor).filter(|c| c.kind() == pair_kind).collect();
    pairs
        .into_iter()
        .find(|p| p.child_by_field_name("key").is_some_and(|k| key_text(k, source) == wanted))
}

fn seq_items<'t>(seq: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = seq.walk();
    let item_kind =
        if seq.kind() == "flow_sequence" { "flow_node" } else { "block_sequence_item" };
    seq.named_children(&mut cursor).filter(|c| c.kind() == item_kind).collect()
}

/// Resolve `path` to the (still-wrapped) value node it denotes.
fn value_node_at<'t>(
    root: Node<'t>,
    source: &str,
    path: &[PathSeg],
) -> Result<Node<'t>, YamlEditError> {
    let mut current = unwrap_node(root);
    for (i, seg) in path.iter().enumerate() {
        let not_found = || YamlEditError::PathNotFound(path_display(&path[..=i]));
        match seg {
            PathSeg::Key(k) => {
                if !current.kind().ends_with("_mapping") {
                    return Err(not_found());
                }
                let pair = find_pair(current, source, k).ok_or_else(not_found)?;
                let value = pair.child_by_field_name("value").ok_or_else(not_found)?;
                if i == path.len() - 1 {
                    return Ok(value);
                }
                current = unwrap_node(value);
            }
            PathSeg::Index(n) => {
                if !current.kind().ends_with("_sequence") {
                    return Err(not_found());
                }
                let items = seq_items(current);
                let item = items.get(*n).copied().ok_or_else(not_found)?;
                // block_sequence_item wraps its value; flow items are values.
                let value = if item.kind() == "block_sequence_item" {
                    item.named_child(0).ok_or_else(not_found)?
                } else {
                    item
                };
                if i == path.len() - 1 {
                    return Ok(value);
                }
                current = unwrap_node(value);
            }
        }
    }
    Ok(root)
}

fn collection_at<'t>(
    root: Node<'t>,
    source: &str,
    path: &[PathSeg],
) -> Result<Node<'t>, YamlEditError> {
    if path.is_empty() {
        return Ok(unwrap_node(root));
    }
    let value = value_node_at(root, source, path)?;
    Ok(unwrap_node(value))
}

// ---------- line helpers ----------

fn newline_style(source: &str) -> &'static str {
    if source.contains("\r\n") { "\r\n" } else { "\n" }
}

fn line_start(source: &str, byte: usize) -> usize {
    source[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Index just past the newline terminating the line containing `byte`.
fn line_end_after(source: &str, byte: usize) -> usize {
    source[byte..].find('\n').map(|i| byte + i + 1).unwrap_or(source.len())
}

fn splice(source: &str, start: usize, end: usize, replacement: &str) -> AppliedEdit {
    let mut new_source = String::with_capacity(source.len() + replacement.len());
    new_source.push_str(&source[..start]);
    new_source.push_str(replacement);
    new_source.push_str(&source[end..]);
    AppliedEdit { new_source, old_range: (start, end), inserted_len: replacement.len() }
}

// ---------- public API ----------

/// Raw scalar text at `path` (quotes included, as written).
pub fn get_scalar(source: &str, path: &[PathSeg]) -> Option<String> {
    let tree = parse(source).ok()?;
    let value = value_node_at(tree.root_node(), source, path).ok()?;
    let scalar = unwrap_node(value);
    matches!(
        scalar.kind(),
        "plain_scalar" | "single_quote_scalar" | "double_quote_scalar" | "block_scalar"
    )
    .then(|| text(scalar, source).to_string())
}

/// Apply one edit and verify it: result must parse (tree-sitter + saphyr) and
/// differ from the input by exactly the intended delta. Worst case is an
/// error — never a mangled document.
pub fn apply(source: &str, edit: &Edit) -> Result<AppliedEdit, YamlEditError> {
    let tree = parse(source)?;
    if has_errors(&tree) {
        return Err(YamlEditError::Parse("document has syntax errors".into()));
    }
    let root = tree.root_node();
    let nl = newline_style(source);

    let applied = match edit {
        Edit::SetScalar { path, value } => {
            let value_node = value_node_at(root, source, path)?;
            let scalar = unwrap_node(value_node);
            match scalar.kind() {
                "plain_scalar" | "single_quote_scalar" | "double_quote_scalar"
                | "block_scalar" => {}
                kind => {
                    return Err(YamlEditError::Unsupported(format!(
                        "value at {} is a {kind}, not a scalar",
                        path_display(path)
                    )));
                }
            }
            splice(source, scalar.start_byte(), scalar.end_byte(), value)
        }
        Edit::InsertMapKey { path, key: new_key, value } => {
            let map = collection_at(root, source, path)?;
            if map.kind() != "block_mapping" {
                return Err(YamlEditError::Unsupported(format!(
                    "{} is a {}, not a block mapping",
                    path_display(path),
                    map.kind()
                )));
            }
            if find_pair(map, source, new_key).is_some() {
                return Err(YamlEditError::DuplicateKey(new_key.clone()));
            }
            let mut cursor = map.walk();
            let last_pair = map
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "block_mapping_pair")
                .last()
                .ok_or_else(|| {
                    YamlEditError::Unsupported("cannot insert into empty mapping".into())
                })?;
            let indent = " ".repeat(last_pair.start_position().column);
            let insert_at = line_end_after(source, last_pair.end_byte());
            let mut piece = String::new();
            if insert_at == source.len() && !source.ends_with('\n') {
                piece.push_str(nl);
            }
            piece.push_str(&indent);
            piece.push_str(new_key);
            piece.push_str(": ");
            piece.push_str(value);
            piece.push_str(nl);
            splice(source, insert_at, insert_at, &piece)
        }
        Edit::InsertMapBlock { path, key: new_key, lines } => {
            let map = collection_at(root, source, path)?;
            if map.kind() != "block_mapping" {
                return Err(YamlEditError::Unsupported(format!(
                    "{} is a {}, not a block mapping",
                    path_display(path),
                    map.kind()
                )));
            }
            if find_pair(map, source, new_key).is_some() {
                return Err(YamlEditError::DuplicateKey(new_key.clone()));
            }
            let mut cursor = map.walk();
            let last_pair = map
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "block_mapping_pair")
                .last()
                .ok_or_else(|| {
                    YamlEditError::Unsupported("cannot insert into empty mapping".into())
                })?;
            let indent = " ".repeat(last_pair.start_position().column);
            let insert_at = line_end_after(source, last_pair.end_byte());
            let mut piece = String::new();
            if insert_at == source.len() && !source.ends_with('\n') {
                piece.push_str(nl);
            }
            piece.push_str(&indent);
            piece.push_str(new_key);
            piece.push(':');
            piece.push_str(nl);
            for line in lines {
                piece.push_str(&indent);
                piece.push_str("  ");
                piece.push_str(line);
                piece.push_str(nl);
            }
            splice(source, insert_at, insert_at, &piece)
        }
        Edit::InsertSeqItem { path, value } => {
            let seq = collection_at(root, source, path)?;
            if seq.kind() != "block_sequence" {
                return Err(YamlEditError::Unsupported(format!(
                    "{} is a {}, not a block sequence",
                    path_display(path),
                    seq.kind()
                )));
            }
            let items = seq_items(seq);
            let last = items.last().copied().ok_or_else(|| {
                YamlEditError::Unsupported("cannot append to empty sequence".into())
            })?;
            let indent = " ".repeat(last.start_position().column);
            let insert_at = line_end_after(source, last.end_byte());
            let mut piece = String::new();
            if insert_at == source.len() && !source.ends_with('\n') {
                piece.push_str(nl);
            }
            piece.push_str(&indent);
            piece.push_str("- ");
            piece.push_str(value);
            piece.push_str(nl);
            splice(source, insert_at, insert_at, &piece)
        }
        Edit::RemoveKey { path } => {
            let (parent_path, last) = path.split_at(path.len().saturating_sub(1));
            let Some(PathSeg::Key(removed_key)) = last.first() else {
                return Err(YamlEditError::Unsupported(
                    "RemoveKey path must end in a key".into(),
                ));
            };
            let map = collection_at(root, source, parent_path)?;
            if map.kind() != "block_mapping" {
                return Err(YamlEditError::Unsupported(format!(
                    "{} is a {}, not a block mapping",
                    path_display(parent_path),
                    map.kind()
                )));
            }
            let mut cursor = map.walk();
            let pair_count = map
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "block_mapping_pair")
                .count();
            if pair_count <= 1 {
                return Err(YamlEditError::Unsupported(
                    "removing the last key would leave an empty mapping".into(),
                ));
            }
            let pair = find_pair(map, source, removed_key)
                .ok_or_else(|| YamlEditError::PathNotFound(path_display(path)))?;
            let start = line_start(source, pair.start_byte());
            let end = line_end_after(source, pair.end_byte());
            splice(source, start, end, "")
        }
        Edit::RenameKey { path, to } => {
            let (parent_path, last) = path.split_at(path.len().saturating_sub(1));
            let Some(PathSeg::Key(from)) = last.first() else {
                return Err(YamlEditError::Unsupported(
                    "RenameKey path must end in a key".into(),
                ));
            };
            let map = collection_at(root, source, parent_path)?;
            if map.kind() != "block_mapping" {
                return Err(YamlEditError::Unsupported(format!(
                    "{} is a {}, not a block mapping",
                    path_display(parent_path),
                    map.kind()
                )));
            }
            // Renaming onto a live key would silently drop one of the two.
            if from != to && find_pair(map, source, to).is_some() {
                return Err(YamlEditError::Unsupported(format!(
                    "{} already has a key {to}",
                    path_display(parent_path)
                )));
            }
            let pair = find_pair(map, source, from)
                .ok_or_else(|| YamlEditError::PathNotFound(path_display(path)))?;
            let key_node = pair
                .child_by_field_name("key")
                .ok_or_else(|| YamlEditError::PathNotFound(path_display(path)))?;
            splice(source, key_node.start_byte(), key_node.end_byte(), to)
        }
        Edit::RemoveSeqItem { path } => {
            let (parent_path, last) = path.split_at(path.len().saturating_sub(1));
            let Some(PathSeg::Index(n)) = last.first() else {
                return Err(YamlEditError::Unsupported(
                    "RemoveSeqItem path must end in an index".into(),
                ));
            };
            let seq = collection_at(root, source, parent_path)?;
            if seq.kind() != "block_sequence" {
                return Err(YamlEditError::Unsupported(format!(
                    "{} is a {}, not a block sequence",
                    path_display(parent_path),
                    seq.kind()
                )));
            }
            let items = seq_items(seq);
            if items.len() <= 1 {
                return Err(YamlEditError::Unsupported(
                    "removing the last item would leave an empty sequence".into(),
                ));
            }
            let item = items
                .get(*n)
                .copied()
                .ok_or_else(|| YamlEditError::PathNotFound(path_display(path)))?;
            let start = line_start(source, item.start_byte());
            let end = line_end_after(source, item.end_byte());
            splice(source, start, end, "")
        }
    };

    // Gate 1: the result must still be syntactically sound in BOTH parsers.
    let new_tree = parse(&applied.new_source)?;
    if has_errors(&new_tree) {
        return Err(YamlEditError::VerifyFailed("edited document no longer parses".into()));
    }
    let old_docs =
        Yaml::load_from_str(source).map_err(|e| YamlEditError::Parse(e.to_string()))?;
    let new_docs = Yaml::load_from_str(&applied.new_source)
        .map_err(|e| YamlEditError::VerifyFailed(format!("saphyr rejects result: {e}")))?;
    let (Some(old_doc), Some(new_doc)) = (old_docs.first(), new_docs.first()) else {
        return Err(YamlEditError::VerifyFailed("empty document".into()));
    };

    // Gate 2: the semantic delta is exactly the intended one. This is what
    // catches anchor/merge-key ripples (ADR-0003) and span-math bugs.
    verify::check_delta(old_doc, new_doc, edit).map_err(YamlEditError::VerifyFailed)?;

    Ok(applied)
}

/// Apply a sequence of edits, each verified individually.
pub fn apply_all(source: &str, edits: &[Edit]) -> Result<String, YamlEditError> {
    let mut current = source.to_string();
    for edit in edits {
        current = apply(&current, edit)?.new_source;
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "services:\n  app:\n    image: repo/app:1 # keep\n    ports:\n      - \"8080:80\"\n      - \"9090:90\"\n  db:\n    image: repo/db:1\n";

    #[test]
    fn set_scalar_preserves_trailing_comment_and_rest() {
        let edit = Edit::SetScalar {
            path: vec![key("services"), key("app"), key("image")],
            value: "repo/app:2".into(),
        };
        let out = apply(DOC, &edit).unwrap().new_source;
        assert!(out.contains("image: repo/app:2 # keep"));
        assert!(out.contains("repo/db:1"));
    }

    #[test]
    fn rename_key_rewrites_only_the_key() {
        let edit = Edit::RenameKey {
            path: vec![key("services"), key("app")],
            to: "my-app.test".into(),
        };
        let out = apply(DOC, &edit).unwrap().new_source;
        assert!(out.contains("  my-app.test:\n"), "{out}");
        assert!(!out.contains("  app:\n"), "{out}");
        // The value keeps its bytes, comment and position; siblings untouched.
        assert!(out.contains("image: repo/app:1 # keep"));
        assert!(out.contains("- \"8080:80\""));
        assert!(out.contains("  db:\n"));
        // Renaming back restores the file exactly.
        let back = Edit::RenameKey {
            path: vec![key("services"), key("my-app.test")],
            to: "app".into(),
        };
        assert_eq!(apply(&out, &back).unwrap().new_source, DOC);
    }

    #[test]
    fn rename_key_refuses_to_clobber_a_sibling() {
        let edit =
            Edit::RenameKey { path: vec![key("services"), key("app")], to: "db".into() };
        assert!(matches!(apply(DOC, &edit), Err(YamlEditError::Unsupported(_))));
    }

    #[test]
    fn rename_key_reports_a_missing_key() {
        let edit =
            Edit::RenameKey { path: vec![key("services"), key("nope")], to: "x".into() };
        assert!(matches!(apply(DOC, &edit), Err(YamlEditError::PathNotFound(_))));
    }

    #[test]
    fn insert_and_remove_map_key_roundtrip_bytes() {
        let insert = Edit::InsertMapKey {
            path: vec![key("services"), key("db")],
            key: "restart".into(),
            value: "unless-stopped".into(),
        };
        let inserted = apply(DOC, &insert).unwrap().new_source;
        assert!(inserted.contains("    restart: unless-stopped\n"));
        let remove =
            Edit::RemoveKey { path: vec![key("services"), key("db"), key("restart")] };
        let removed = apply(&inserted, &remove).unwrap().new_source;
        assert_eq!(removed, DOC, "insert+remove must restore the exact bytes");
    }

    #[test]
    fn insert_map_block_and_remove_roundtrip_bytes() {
        let insert = Edit::InsertMapBlock {
            path: vec![key("services")],
            key: "redis".into(),
            lines: vec![
                "image: 'redis:alpine'".into(),
                "ports:".into(),
                "  - '6379:6379'".into(),
                "healthcheck:".into(),
                "  retries: 3".into(),
            ],
        };
        let inserted = apply(DOC, &insert).unwrap().new_source;
        assert!(inserted.contains("  redis:\n    image: 'redis:alpine'\n    ports:\n      - '6379:6379'\n"));
        assert!(inserted.contains("    healthcheck:\n      retries: 3\n"));
        let remove = Edit::RemoveKey { path: vec![key("services"), key("redis")] };
        assert_eq!(apply(&inserted, &remove).unwrap().new_source, DOC);
    }

    #[test]
    fn insert_map_block_refuses_duplicate_key() {
        let dup = Edit::InsertMapBlock {
            path: vec![key("services")],
            key: "db".into(),
            lines: vec!["image: x".into()],
        };
        assert!(matches!(apply(DOC, &dup), Err(YamlEditError::DuplicateKey(_))));
    }

    #[test]
    fn seq_insert_and_remove() {
        let insert = Edit::InsertSeqItem {
            path: vec![key("services"), key("app"), key("ports")],
            value: "\"7070:70\"".into(),
        };
        let inserted = apply(DOC, &insert).unwrap().new_source;
        assert!(inserted.contains("      - \"7070:70\"\n"));
        let remove = Edit::RemoveSeqItem {
            path: vec![key("services"), key("app"), key("ports"), index(2)],
        };
        assert_eq!(apply(&inserted, &remove).unwrap().new_source, DOC);
    }

    #[test]
    fn refusals_are_clean_errors() {
        let dup = Edit::InsertMapKey {
            path: vec![key("services"), key("app")],
            key: "image".into(),
            value: "x".into(),
        };
        assert!(matches!(apply(DOC, &dup), Err(YamlEditError::DuplicateKey(_))));
        let missing = Edit::SetScalar {
            path: vec![key("services"), key("nope"), key("image")],
            value: "x".into(),
        };
        assert!(matches!(apply(DOC, &missing), Err(YamlEditError::PathNotFound(_))));
        let empty = Edit::RemoveKey { path: vec![key("services"), key("db"), key("image")] };
        assert!(matches!(apply(DOC, &empty), Err(YamlEditError::Unsupported(_))));
        let nonscalar =
            Edit::SetScalar { path: vec![key("services"), key("app")], value: "x".into() };
        assert!(matches!(apply(DOC, &nonscalar), Err(YamlEditError::Unsupported(_))));
    }

    #[test]
    fn anchor_ripple_is_refused_by_the_delta_gate() {
        let doc = "base: &b\n  restart: always\nservices:\n  app:\n    <<: *b\n    image: x\n  extra: ok\n";
        let edit =
            Edit::SetScalar { path: vec![key("base"), key("restart")], value: "never".into() };
        assert!(matches!(apply(doc, &edit), Err(YamlEditError::VerifyFailed(_))));
    }

    #[test]
    fn crlf_and_missing_trailing_newline_survive() {
        let crlf = DOC.replace('\n', "\r\n");
        let edit = Edit::InsertMapKey {
            path: vec![key("services"), key("db")],
            key: "restart".into(),
            value: "always".into(),
        };
        let out = apply(&crlf, &edit).unwrap().new_source;
        assert!(out.contains("    restart: always\r\n"));
        assert!(!out.replace("\r\n", "").contains('\n'), "no bare LF introduced");

        let no_nl = DOC.trim_end().to_string();
        let out = apply(&no_nl, &edit).unwrap().new_source;
        assert!(out.contains("    restart: always\n"));
    }

    #[test]
    fn get_scalar_returns_raw_text() {
        assert_eq!(
            get_scalar(DOC, &[key("services"), key("app"), key("ports"), index(0)]).as_deref(),
            Some("\"8080:80\"")
        );
        assert_eq!(get_scalar(DOC, &[key("nope")]), None);
    }
}
