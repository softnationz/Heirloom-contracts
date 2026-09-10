//! Automated documentation generation from source code.
//!
//! Hand-written documentation drifts from the code it describes: signatures
//! change, parameters are renamed, examples stop compiling. This module
//! generates reference documentation directly from Rust sources, so the
//! output can be regenerated in CI and diffed against the committed docs.
//!
//! # Architecture
//!
//! ```text
//! source text ─→ parse_module   → ModuleDoc { summary, items }
//!                extract_examples → Example { name, code }   (from #[test] fns)
//!                attach_examples  → items carry their examples
//!                render_markdown  → docs/<module>.md
//! ```
//!
//! Parsing is deliberately lexical rather than a full syntax tree: it only
//! needs public item headers, doc comments, and test bodies, and staying
//! text-based keeps the generator dependency-free.

use serde::{Deserialize, Serialize};

// === Types

/// The kind of public item a documented entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Function,
    Struct,
    Enum,
    Trait,
    TypeAlias,
}

impl ItemKind {
    /// Heading label used when rendering markdown.
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Function => "fn",
            ItemKind::Struct => "struct",
            ItemKind::Enum => "enum",
            ItemKind::Trait => "trait",
            ItemKind::TypeAlias => "type",
        }
    }
}

/// One parameter of a documented function, with its declared type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamDoc {
    pub name: String,
    pub ty: String,
}

/// A usage example lifted from a test function body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Example {
    /// Name of the test the example came from, kept so readers can find it.
    pub name: String,
    pub code: String,
}

/// A single documented public item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocItem {
    pub name: String,
    pub kind: ItemKind,
    /// The item header, normalized to a single line.
    pub signature: String,
    /// Doc comment lines (`///`), with the leading marker stripped.
    pub doc: Vec<String>,
    pub params: Vec<ParamDoc>,
    pub returns: Option<String>,
    pub examples: Vec<Example>,
}

/// Generated documentation for one module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDoc {
    pub module: String,
    /// Module-level doc comment lines (`//!`).
    pub summary: Vec<String>,
    pub items: Vec<DocItem>,
}

// === Parsing

/// Parse a module's source text into its documentation model.
pub fn parse_module(module: &str, source: &str) -> ModuleDoc {
    let mut doc = ModuleDoc {
        module: module.to_string(),
        summary: module_summary(source),
        items: Vec::new(),
    };

    let lines: Vec<&str> = source.lines().collect();
    let mut pending_doc: Vec<String> = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let trimmed = lines[index].trim();

        if let Some(rest) = trimmed.strip_prefix("///") {
            pending_doc.push(rest.trim().to_string());
            index += 1;
            continue;
        }

        // Attributes sit between the doc comment and the item it belongs to,
        // so they must not clear the pending doc lines.
        if trimmed.starts_with("#[") || trimmed.starts_with("//!") {
            index += 1;
            continue;
        }

        match item_kind_of(trimmed) {
            Some(kind) => {
                let (header, next) = collect_header(&lines, index);
                if let Some(name) = item_name(&header, kind) {
                    doc.items.push(DocItem {
                        name,
                        kind,
                        signature: normalize_whitespace(&header),
                        doc: std::mem::take(&mut pending_doc),
                        params: if kind == ItemKind::Function {
                            parse_params(&header)
                        } else {
                            Vec::new()
                        },
                        returns: if kind == ItemKind::Function {
                            parse_return(&header)
                        } else {
                            None
                        },
                        examples: Vec::new(),
                    });
                }
                index = next;
            }
            None => {
                if !trimmed.is_empty() {
                    pending_doc.clear();
                }
                index += 1;
            }
        }
    }

    doc
}

fn module_summary(source: &str) -> Vec<String> {
    source
        .lines()
        .take_while(|line| {
            let trimmed = line.trim();
            trimmed.is_empty() || trimmed.starts_with("//!")
        })
        .filter_map(|line| line.trim().strip_prefix("//!").map(|s| s.trim().to_string()))
        .collect()
}

fn item_kind_of(trimmed: &str) -> Option<ItemKind> {
    let rest = trimmed.strip_prefix("pub ")?;
    // `pub(crate)` and friends are treated as private for reference docs.
    let rest = rest.trim_start();
    for prefix in ["async fn ", "fn "] {
        if let Some(after) = rest.strip_prefix(prefix) {
            // Skip declarations without a name (should not occur, but the
            // lexical parse cannot assume well-formed input).
            if !after.trim().is_empty() {
                return Some(ItemKind::Function);
            }
        }
    }
    if rest.starts_with("struct ") {
        return Some(ItemKind::Struct);
    }
    if rest.starts_with("enum ") {
        return Some(ItemKind::Enum);
    }
    if rest.starts_with("trait ") {
        return Some(ItemKind::Trait);
    }
    if rest.starts_with("type ") {
        return Some(ItemKind::TypeAlias);
    }
    None
}

/// Collect an item header across however many lines it spans, returning the
/// header text and the index of the line after it.
fn collect_header(lines: &[&str], start: usize) -> (String, usize) {
    let mut header = String::new();
    let mut index = start;

    while index < lines.len() {
        if !header.is_empty() {
            header.push(' ');
        }
        header.push_str(lines[index].trim());
        index += 1;

        if let Some(end) = header_end(&header) {
            header.truncate(end);
            break;
        }
    }

    (header.trim().to_string(), index)
}

/// Index at which the header stops: the opening brace of a body, or the
/// semicolon of a declaration, whichever comes first outside parentheses.
fn header_end(header: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (position, ch) in header.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '{' | ';' if depth == 0 => return Some(position),
            _ => {}
        }
    }
    None
}

fn item_name(header: &str, kind: ItemKind) -> Option<String> {
    let keyword = match kind {
        ItemKind::Function => "fn ",
        ItemKind::Struct => "struct ",
        ItemKind::Enum => "enum ",
        ItemKind::Trait => "trait ",
        ItemKind::TypeAlias => "type ",
    };
    let after = header.split_once(keyword)?.1;
    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Extract the parameter list, skipping receivers (`self`) and bindings that
/// are destructured patterns rather than plain names.
fn parse_params(header: &str) -> Vec<ParamDoc> {
    let Some(open) = header.find('(') else {
        return Vec::new();
    };
    let inside = match matching_paren(header, open) {
        Some(close) => &header[open + 1..close],
        None => &header[open + 1..],
    };

    split_top_level(inside)
        .into_iter()
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() || part.ends_with("self") {
                return None;
            }
            let (name, ty) = split_binding(part)?;
            Some(ParamDoc {
                name: normalize_whitespace(name),
                ty: normalize_whitespace(ty),
            })
        })
        .collect()
}

/// Split `name: Type` at the colon that separates the binding from its type,
/// ignoring colons inside generics or paths.
fn split_binding(part: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let bytes: Vec<char> = part.chars().collect();
    for (position, ch) in part.char_indices() {
        let index = part[..position].chars().count();
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                // A path separator (`::`) is never the binding colon.
                if bytes.get(index + 1) == Some(&':') {
                    continue;
                }
                return Some((&part[..position], &part[position + 1..]));
            }
            _ => {}
        }
    }
    None
}

fn parse_return(header: &str) -> Option<String> {
    let after = header.split("->").nth(1)?;
    let cleaned = after.split(" where ").next().unwrap_or(after);
    let cleaned = normalize_whitespace(cleaned);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (position, ch) in text.char_indices().skip(open) {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(position);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (position, ch) in text.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..position]);
                start = position + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// === Example extraction

/// Extract examples from test source: every `#[test]` (or `#[tokio::test]`)
/// function becomes a candidate example keyed by its own name.
pub fn extract_examples(source: &str) -> Vec<Example> {
    let lines: Vec<&str> = source.lines().collect();
    let mut examples = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let trimmed = lines[index].trim();
        let is_test_attr = trimmed.starts_with("#[test]")
            || (trimmed.starts_with("#[") && trimmed.contains("::test"));
        if !is_test_attr {
            index += 1;
            continue;
        }

        // Walk forward to the function header the attribute decorates.
        let mut cursor = index + 1;
        while cursor < lines.len() && !lines[cursor].contains("fn ") {
            cursor += 1;
        }
        if cursor >= lines.len() {
            break;
        }

        let (header, body_start) = collect_header(&lines, cursor);
        let Some(name) = item_name(&header, ItemKind::Function) else {
            index = cursor + 1;
            continue;
        };
        let (code, next) = collect_body(&lines, body_start.saturating_sub(1));
        examples.push(Example { name, code });
        index = next;
    }

    examples
}

/// Collect the brace-delimited body starting at or after `start`, dedented
/// to the innermost common indentation.
fn collect_body(lines: &[&str], start: usize) -> (String, usize) {
    let mut depth = 0usize;
    let mut opened = false;
    let mut collected: Vec<&str> = Vec::new();
    let mut index = start;

    while index < lines.len() {
        let line = lines[index];
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }

        if opened {
            if depth == 0 {
                index += 1;
                break;
            }
            // The line holding the opening brace is the header, not body.
            if !collected.is_empty() || !line.contains('{') {
                collected.push(line);
            }
        }
        index += 1;
    }

    (dedent(&collected), index)
}

fn dedent(lines: &[&str]) -> String {
    let indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);

    lines
        .iter()
        .map(|line| {
            if line.len() >= indent {
                &line[indent..]
            } else {
                line.trim_start()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// Attach examples to the items they exercise, matching a test name against
/// the item name (`fn evaluate_flag` gets `test_evaluate_flag_defaults`).
pub fn attach_examples(doc: &mut ModuleDoc, examples: &[Example]) {
    for item in &mut doc.items {
        item.examples = examples
            .iter()
            .filter(|example| example.name.contains(&item.name))
            .cloned()
            .collect();
    }
}

// === Rendering

/// Render a module's documentation as markdown.
pub fn render_markdown(doc: &ModuleDoc) -> String {
    let mut out = format!("# Module `{}`\n", doc.module);

    if !doc.summary.is_empty() {
        out.push('\n');
        out.push_str(&doc.summary.join("\n"));
        out.push('\n');
    }

    for item in &doc.items {
        out.push_str(&format!(
            "\n## `{} {}`\n\n```rust\n{}\n```\n",
            item.kind.label(),
            item.name,
            item.signature
        ));

        if !item.doc.is_empty() {
            out.push('\n');
            out.push_str(&item.doc.join("\n"));
            out.push('\n');
        }

        if !item.params.is_empty() {
            out.push_str("\n**Parameters**\n\n");
            for param in &item.params {
                out.push_str(&format!("- `{}`: `{}`\n", param.name, param.ty));
            }
        }

        if let Some(returns) = &item.returns {
            out.push_str(&format!("\n**Returns**: `{returns}`\n"));
        }

        for example in &item.examples {
            out.push_str(&format!(
                "\n**Example** (from `{}`)\n\n```rust\n{}\n```\n",
                example.name, example.code
            ));
        }
    }

    out
}

/// Generate markdown for a module in one call: parse the source, pull
/// examples from the test source, and render.
pub fn generate(module: &str, source: &str, test_source: Option<&str>) -> String {
    let mut doc = parse_module(module, source);
    if let Some(tests) = test_source {
        let examples = extract_examples(tests);
        attach_examples(&mut doc, &examples);
    }
    render_markdown(&doc)
}

// === Tests

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
//! Feature flag evaluation.
//! Supports percentage rollouts.

use std::sync::Arc;

/// A single flag definition.
#[derive(Debug, Clone)]
pub struct Flag {
    pub key: String,
}

/// Evaluate a flag for one subject.
/// Returns false when the flag is unknown.
#[allow(dead_code)]
pub async fn evaluate_flag(
    state: Arc<FlagState>,
    key: &str,
    rollout: u8,
) -> Result<bool, Error> {
    Ok(true)
}

fn private_helper(x: u8) -> u8 {
    x
}
"#;

    const TEST_SOURCE: &str = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn test_evaluate_flag_returns_true() {
        let state = Arc::new(FlagState::default());
        assert!(evaluate_flag(state, "beta", 100).await.unwrap());
    }

    #[tokio::test]
    async fn test_unrelated_thing() {
        assert_eq!(1, 1);
    }
}
"#;

    #[test]
    fn parse_module_captures_module_summary() {
        let doc = parse_module("feature_flags", SOURCE);
        assert_eq!(
            doc.summary,
            vec![
                "Feature flag evaluation.".to_string(),
                "Supports percentage rollouts.".to_string()
            ]
        );
    }

    #[test]
    fn parse_module_skips_private_items() {
        let doc = parse_module("feature_flags", SOURCE);
        let names: Vec<&str> = doc.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["Flag", "evaluate_flag"]);
    }

    #[test]
    fn parse_module_extracts_signature_across_lines() {
        let doc = parse_module("feature_flags", SOURCE);
        let item = doc.items.iter().find(|i| i.name == "evaluate_flag").unwrap();
        assert_eq!(
            item.signature,
            "pub async fn evaluate_flag( state: Arc<FlagState>, key: &str, rollout: u8, ) -> Result<bool, Error>"
        );
        assert_eq!(item.kind, ItemKind::Function);
    }

    #[test]
    fn parse_module_extracts_params_and_return() {
        let doc = parse_module("feature_flags", SOURCE);
        let item = doc.items.iter().find(|i| i.name == "evaluate_flag").unwrap();
        assert_eq!(
            item.params,
            vec![
                ParamDoc {
                    name: "state".into(),
                    ty: "Arc<FlagState>".into()
                },
                ParamDoc {
                    name: "key".into(),
                    ty: "&str".into()
                },
                ParamDoc {
                    name: "rollout".into(),
                    ty: "u8".into()
                },
            ]
        );
        assert_eq!(item.returns.as_deref(), Some("Result<bool, Error>"));
    }

    #[test]
    fn parse_module_keeps_doc_comments_through_attributes() {
        let doc = parse_module("feature_flags", SOURCE);
        let item = doc.items.iter().find(|i| i.name == "evaluate_flag").unwrap();
        assert_eq!(
            item.doc,
            vec![
                "Evaluate a flag for one subject.".to_string(),
                "Returns false when the flag is unknown.".to_string()
            ]
        );
    }

    #[test]
    fn struct_items_carry_no_params() {
        let doc = parse_module("feature_flags", SOURCE);
        let item = doc.items.iter().find(|i| i.name == "Flag").unwrap();
        assert_eq!(item.kind, ItemKind::Struct);
        assert!(item.params.is_empty());
        assert!(item.returns.is_none());
    }

    #[test]
    fn extract_examples_reads_both_test_flavors() {
        let examples = extract_examples(TEST_SOURCE);
        let names: Vec<&str> = examples.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["test_evaluate_flag_returns_true", "test_unrelated_thing"]
        );
        assert!(examples[0].code.starts_with("let state ="));
        assert!(examples[0].code.contains("assert!(evaluate_flag"));
    }

    #[test]
    fn attach_examples_matches_by_item_name() {
        let mut doc = parse_module("feature_flags", SOURCE);
        let examples = extract_examples(TEST_SOURCE);
        attach_examples(&mut doc, &examples);

        let item = doc.items.iter().find(|i| i.name == "evaluate_flag").unwrap();
        assert_eq!(item.examples.len(), 1);
        assert_eq!(item.examples[0].name, "test_evaluate_flag_returns_true");

        let flag = doc.items.iter().find(|i| i.name == "Flag").unwrap();
        assert!(flag.examples.is_empty());
    }

    #[test]
    fn generate_renders_full_markdown() {
        let markdown = generate("feature_flags", SOURCE, Some(TEST_SOURCE));
        assert!(markdown.starts_with("# Module `feature_flags`"));
        assert!(markdown.contains("## `fn evaluate_flag`"));
        assert!(markdown.contains("- `rollout`: `u8`"));
        assert!(markdown.contains("**Returns**: `Result<bool, Error>`"));
        assert!(markdown.contains("**Example** (from `test_evaluate_flag_returns_true`)"));
    }

    #[test]
    fn generate_without_tests_omits_examples() {
        let markdown = generate("feature_flags", SOURCE, None);
        assert!(!markdown.contains("**Example**"));
    }
}
