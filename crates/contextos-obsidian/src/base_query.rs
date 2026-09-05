use std::cmp::Ordering;

use serde_json::{Map, Value};
use thiserror::Error;

use crate::base::BaseDocument;

/// Scalar `file.*` metadata `base_query` can filter, sort, or display on.
/// `name` is the full filename including its extension (`alpha.md`),
/// matching Obsidian's own `file.name`; `basename` is the name without an
/// extension (`alpha`). `folder` is the vault-relative parent directory of
/// `path` (`""` for a vault-root note). `ctime`/`mtime` are the platform's
/// rendered timestamp text (same `OffsetDateTime::to_string()` convention
/// `fs_get_file_info` already uses), `None` when the platform reports none;
/// only equality/inequality is supported on them, never ordering or
/// arithmetic, consistent with the rest of `base_query`'s filter grammar.
#[derive(Clone, Copy, Debug)]
pub struct FileMetadata<'a> {
    pub name: &'a str,
    pub basename: &'a str,
    pub path: &'a str,
    pub ext: &'a str,
    pub folder: &'a str,
    pub size: u64,
    pub ctime: Option<&'a str>,
    pub mtime: Option<&'a str>,
}

/// One candidate row: a note's frontmatter, `file.*` metadata, resolved
/// tags (frontmatter `tags` plus inline `#tag`, via
/// [`contextos_core::extract_tags`]), and the note's outgoing links,
/// outgoing embeds, and backlinks, ready for filter evaluation. `links`/
/// `embeds` are literal wikilink target text parsed from the note's own
/// body (matching `links_read`'s existing `outgoing[].target`
/// representation, not resolved through the link graph, so they work
/// without one); `backlinks` are vault-relative paths resolved through the
/// link graph when one is available, empty otherwise (see
/// `contextos-mcp`'s orchestration for how each is populated).
#[derive(Clone, Copy, Debug)]
pub struct RowContext<'a> {
    pub frontmatter: &'a Map<String, Value>,
    pub file: FileMetadata<'a>,
    pub tags: &'a [String],
    pub links: &'a [String],
    pub embeds: &'a [String],
    pub backlinks: &'a [String],
}

/// A sort key resolved from a view's `sort` list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortKey {
    pub property: String,
    pub descending: bool,
}

/// The effective filter/column/sort/limit shape `base_query` executes,
/// resolved from either an existing `.base` file's named view
/// ([`QueryDefinition::from_document`]) or an inline ad hoc definition
/// ([`QueryDefinition::from_inline`]).
#[derive(Clone, Debug)]
pub struct QueryDefinition {
    pub filters: Option<Value>,
    pub columns: Vec<String>,
    pub sort: Vec<SortKey>,
    pub limit: Option<usize>,
    /// The document's top-level `formulas` map (name to expression string),
    /// empty when absent: `formulas` is a document-level-only key, never
    /// per-view, matching Obsidian's own Bases schema. Consulted only when
    /// resolving a `formula.*` display column ([`resolve_column`]); a
    /// `formula.*` reference in a filter or sort key is unaffected and
    /// still fails closed.
    pub formulas: Map<String, Value>,
}

impl QueryDefinition {
    /// Resolves the named view (or the first view when `view` is `None`)
    /// from an already schema-validated [`BaseDocument`].
    ///
    /// # Errors
    ///
    /// Returns [`BaseQueryError::ViewNotFound`] when `view` names a view the
    /// document does not contain, or [`BaseQueryError::NoColumns`] when
    /// neither the resolved view's `order` nor the document's top-level
    /// `properties` yield a display column set.
    pub fn from_document(document: &BaseDocument, view: Option<&str>) -> Result<Self, BaseQueryError> {
        let definition = document.definition();
        let views = definition
            .get("views")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let selected = match view {
            Some(name) => views
                .iter()
                .find(|candidate| candidate.get("name").and_then(Value::as_str) == Some(name))
                .ok_or_else(|| BaseQueryError::ViewNotFound { name: name.to_owned() })?,
            None => views.first().ok_or_else(|| BaseQueryError::MalformedDefinition {
                path: "views".to_owned(),
                violation: "base has no views to query".to_owned(),
            })?,
        };
        let selected_name = selected
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)")
            .to_owned();
        let selected = selected
            .as_object()
            .ok_or_else(|| BaseQueryError::MalformedDefinition {
                path: "views".to_owned(),
                violation: "view must be an object".to_owned(),
            })?;
        let filters = merge_and(definition.get("filters"), selected.get("filters"));
        let order = string_array(selected.get("order"), "views.order")?;
        let sort = parse_sort(selected.get("sort"))?;
        let limit = parse_limit(selected.get("limit"))?;
        let columns = if order.is_empty() {
            property_keys(definition.get("properties"))
        } else {
            order
        };
        if columns.is_empty() {
            return Err(BaseQueryError::NoColumns { view: selected_name });
        }
        let formulas = formula_map(definition.get("formulas"));
        Ok(Self {
            filters,
            columns,
            sort,
            limit,
            formulas,
        })
    }

    /// Resolves an inline ad hoc definition: the same `filters`/`order`/
    /// `sort`/`limit`/`properties` shape as one view, without a wrapping
    /// `views` array.
    ///
    /// # Errors
    ///
    /// Returns [`BaseQueryError::MalformedDefinition`] for a structurally
    /// invalid `order`, `sort`, or `limit`, or [`BaseQueryError::NoColumns`]
    /// when neither `order` nor `properties` yield a display column set.
    pub fn from_inline(definition: &Map<String, Value>) -> Result<Self, BaseQueryError> {
        let filters = definition.get("filters").cloned();
        let order = string_array(definition.get("order"), "order")?;
        let sort = parse_sort(definition.get("sort"))?;
        let limit = parse_limit(definition.get("limit"))?;
        let columns = if order.is_empty() {
            property_keys(definition.get("properties"))
        } else {
            order
        };
        if columns.is_empty() {
            return Err(BaseQueryError::NoColumns {
                view: "(inline)".to_owned(),
            });
        }
        let formulas = formula_map(definition.get("formulas"));
        Ok(Self {
            filters,
            columns,
            sort,
            limit,
            formulas,
        })
    }
}

/// Reads a `formulas` map's string-valued entries only: a non-string
/// formula body (structurally invalid per the schema) is dropped rather
/// than rejecting the whole query, since [`resolve_formula`] degrades to
/// the "not evaluated" marker for any formula it does not recognise
/// anyway.
fn formula_map(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .map(|formulas| {
            formulas
                .iter()
                .filter(|(_, expression)| expression.is_string())
                .map(|(name, expression)| (name.clone(), expression.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn merge_and(top: Option<&Value>, view: Option<&Value>) -> Option<Value> {
    match (top, view) {
        (Some(top), Some(view)) => Some(Value::Object(Map::from_iter([(
            "and".to_owned(),
            Value::Array(vec![top.clone(), view.clone()]),
        )]))),
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (None, None) => None,
    }
}

fn string_array(value: Option<&Value>, path: &str) -> Result<Vec<String>, BaseQueryError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| BaseQueryError::MalformedDefinition {
        path: path.to_owned(),
        violation: "must be an array of strings".to_owned(),
    })?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| BaseQueryError::MalformedDefinition {
                    path: path.to_owned(),
                    violation: "every entry must be a string".to_owned(),
                })
        })
        .collect()
}

fn property_keys(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

fn parse_sort(value: Option<&Value>) -> Result<Vec<SortKey>, BaseQueryError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or_else(|| BaseQueryError::MalformedDefinition {
        path: "sort".to_owned(),
        violation: "must be an array".to_owned(),
    })?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let path = format!("sort[{index}]");
            let entry = entry.as_object().ok_or_else(|| BaseQueryError::MalformedDefinition {
                path: path.clone(),
                violation: "sort entry must be an object".to_owned(),
            })?;
            let property =
                entry
                    .get("property")
                    .and_then(Value::as_str)
                    .ok_or_else(|| BaseQueryError::MalformedDefinition {
                        path: format!("{path}.property"),
                        violation: "sort property must be a string".to_owned(),
                    })?;
            validate_property_name(property, &format!("{path}.property"))?;
            let descending = match entry.get("direction").and_then(Value::as_str) {
                Some("DESC") => true,
                Some("ASC") | None => false,
                Some(_) => {
                    return Err(BaseQueryError::MalformedDefinition {
                        path: format!("{path}.direction"),
                        violation: "direction must be ASC or DESC".to_owned(),
                    });
                }
            };
            Ok(SortKey {
                property: property.to_owned(),
                descending,
            })
        })
        .collect()
}

fn parse_limit(value: Option<&Value>) -> Result<Option<usize>, BaseQueryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let limit = value
        .as_u64()
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|limit| *limit > 0)
        .ok_or_else(|| BaseQueryError::MalformedDefinition {
            path: "limit".to_owned(),
            violation: "limit must be a positive integer".to_owned(),
        })?;
    Ok(Some(limit))
}

/// A vault-scan-root narrowing hint found in a filter tree: a directory
/// taken directly from a `file.folder == "..."` leaf, or a file path whose
/// parent directory the caller must derive from a `file.path == "..."`
/// leaf. Two variants rather than one pre-resolved directory, so the
/// zero-copy borrow from the filter tree is preserved and the caller
/// decides how to turn each into a scan root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanRootHint<'a> {
    Folder(&'a str),
    Path(&'a str),
}

/// Returns a provably safe vault-scan-root narrowing hint for `filters`, or
/// `None` when no such hint can be established. Purely an optimisation
/// hint: every candidate file still passes the full filter tree, so a
/// caller ignoring this or scanning a broader root than it suggests cannot
/// change which rows match — but a hint this function does return must
/// never cause a genuine match to be skipped, which is why `and`/`or`/`not`
/// (at both the JSON-tree level a merged view's filters take, and the
/// `&&`/`||`/`!`/`(...)` level one filter string's own leaves combine at)
/// apply different rules:
///
/// - `and`: any single narrowed conjunct safely bounds the whole
///   conjunction, since every conjunct must hold for a match.
/// - `or`: safe only when every operand agrees on the identical hint — an
///   operand with no hint, or a different one, means a match could satisfy
///   the `or` from outside any single narrowed root.
/// - `not`: never narrowed. A hint on the negated operand describes where
///   matches *of that operand* live, not matches of its negation.
#[must_use]
pub fn scan_root_hint(filters: Option<&Value>) -> Option<ScanRootHint<'_>> {
    let filters = filters?;
    if let Some(expr) = filters.as_str() {
        return expression_scan_hint(expr);
    }
    let object = filters.as_object()?;
    let (operator, operands) = object.iter().next()?;
    let operands = operands.as_array()?;
    match operator.as_str() {
        "and" => operands.iter().find_map(|operand| scan_root_hint(Some(operand))),
        "or" => merge_or_hints(operands.iter().map(|operand| scan_root_hint(Some(operand)))),
        _ => None,
    }
}

/// Parses `expr` with the identical `&&`/`||`/`!`/`(...)` grammar and
/// precedence [`evaluate_expression`] evaluates, resolving to a narrowing
/// hint (via [`leaf_scan_hint`] at each leaf) instead of a boolean, and
/// applying [`scan_root_hint`]'s own and/or/not safety rules at the string
/// level too. An expression this walker cannot fully parse (anything
/// outside the documented grammar, including a genuine syntax error)
/// yields no hint rather than guessing: `evaluate_filters` still runs the
/// real grammar and reports any syntax error itself.
fn expression_scan_hint(expr: &str) -> Option<ScanRootHint<'_>> {
    let mut cursor = ExpressionCursor { text: expr, pos: 0 };
    let hint = hint_or(&mut cursor);
    cursor.skip_whitespace();
    if cursor.pos != cursor.text.len() {
        return None;
    }
    hint
}

fn hint_or<'a>(cursor: &mut ExpressionCursor<'a>) -> Option<ScanRootHint<'a>> {
    let mut hints = vec![hint_and(cursor)];
    loop {
        cursor.skip_whitespace();
        if !cursor.rest().starts_with("||") {
            return merge_or_hints(hints.into_iter());
        }
        cursor.pos += 2;
        hints.push(hint_and(cursor));
    }
}

fn hint_and<'a>(cursor: &mut ExpressionCursor<'a>) -> Option<ScanRootHint<'a>> {
    let mut hint = hint_not(cursor);
    loop {
        cursor.skip_whitespace();
        if !cursor.rest().starts_with("&&") {
            return hint;
        }
        cursor.pos += 2;
        let rhs = hint_not(cursor);
        hint = hint.or(rhs);
    }
}

fn hint_not<'a>(cursor: &mut ExpressionCursor<'a>) -> Option<ScanRootHint<'a>> {
    cursor.skip_whitespace();
    if cursor.rest().starts_with('!') && !cursor.rest().starts_with("!=") {
        cursor.pos += 1;
        // Still walked, purely to advance the cursor past the negated
        // operand's own span (matching `parse_not`'s recursion): a negated
        // leaf's hint is never safe to propagate, so the result itself is
        // discarded.
        let _ = hint_not(cursor);
        return None;
    }
    hint_primary(cursor)
}

fn hint_primary<'a>(cursor: &mut ExpressionCursor<'a>) -> Option<ScanRootHint<'a>> {
    cursor.skip_whitespace();
    if cursor.rest().starts_with('(') {
        cursor.pos += 1;
        let hint = hint_or(cursor);
        cursor.skip_whitespace();
        if !cursor.rest().starts_with(')') {
            return None;
        }
        cursor.pos += 1;
        return hint;
    }
    let leaf = scan_leaf(cursor);
    leaf_scan_hint(leaf.trim())
}

/// Applies [`scan_root_hint`]'s `or` safety rule: a hint only when every
/// operand yielded a hint and every one is identical.
fn merge_or_hints<'a>(mut hints: impl Iterator<Item = Option<ScanRootHint<'a>>>) -> Option<ScanRootHint<'a>> {
    let first = hints.next()??;
    hints.all(|hint| hint == Some(first)).then_some(first)
}

/// Only an equality leaf on `file.path` or `file.folder`, or a
/// `file.inFolder(...)` call, is eligible: equality anchors the whole
/// string, and `inFolder` is anchored at the start of the candidate's own
/// folder by definition (see [`in_folder`]), so either is a provably safe
/// scan root (as a parent directory for `file.path`, directly for
/// `file.folder`/`file.inFolder`). `.contains()` is deliberately excluded
/// from all of these, even though it is a supported filter leaf: a
/// substring can appear anywhere in a path or folder name
/// (`contains("archive")` matches `notes/archive-2024.md`, outside any
/// `archive/` directory), so narrowing the scan on it could silently drop
/// genuine matches rather than merely cost a slower scan.
fn leaf_scan_hint(expr: &str) -> Option<ScanRootHint<'_>> {
    let trimmed = expr.trim();
    if let Some(inner) = trimmed
        .strip_prefix("file.inFolder(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return parse_quoted(inner.trim()).map(ScanRootHint::Folder);
    }
    let (property, operator, rhs) = parse_comparison(trimmed)?;
    if !matches!(operator, ComparisonOp::Equal) {
        return None;
    }
    match property {
        "file.path" => parse_quoted(rhs).map(ScanRootHint::Path),
        "file.folder" => parse_quoted(rhs).map(ScanRootHint::Folder),
        _ => None,
    }
}

/// Evaluates a resolved filter tree (`None` matches every row) against one
/// candidate row.
///
/// # Errors
///
/// Fails closed: returns [`BaseQueryError::UnsupportedFilterExpression`] for
/// any leaf outside the documented grammar, [`BaseQueryError::FormulaReference`]
/// for any `formula.*` reference, and [`BaseQueryError::UnsupportedFileProperty`]
/// for any `file.*` accessor outside Obsidian's own documented set (every
/// one of which `base_query` now supports).
pub fn evaluate_filters(filters: Option<&Value>, row: &RowContext<'_>) -> Result<bool, BaseQueryError> {
    let Some(filters) = filters else {
        return Ok(true);
    };
    evaluate_node(filters, row, "filters")
}

fn evaluate_node(node: &Value, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    if let Some(expr) = node.as_str() {
        return evaluate_expression(expr, row, path);
    }
    let object = node.as_object().ok_or_else(|| BaseQueryError::MalformedDefinition {
        path: path.to_owned(),
        violation: "filter node must be a string or an and/or/not object".to_owned(),
    })?;
    let (operator, operands) = object
        .iter()
        .next()
        .ok_or_else(|| BaseQueryError::MalformedDefinition {
            path: path.to_owned(),
            violation: "filter object must contain and, or, or not".to_owned(),
        })?;
    let operands = operands.as_array().ok_or_else(|| BaseQueryError::MalformedDefinition {
        path: format!("{path}.{operator}"),
        violation: "filter operands must be an array".to_owned(),
    })?;
    match operator.as_str() {
        "and" => {
            let mut result = true;
            for (index, operand) in operands.iter().enumerate() {
                let value = evaluate_node(operand, row, &format!("{path}.and[{index}]"))?;
                result = result && value;
            }
            Ok(result)
        }
        "or" => {
            let mut result = false;
            for (index, operand) in operands.iter().enumerate() {
                let value = evaluate_node(operand, row, &format!("{path}.or[{index}]"))?;
                result = result || value;
            }
            Ok(result)
        }
        "not" => {
            let mut result = true;
            for (index, operand) in operands.iter().enumerate() {
                let value = evaluate_node(operand, row, &format!("{path}.not[{index}]"))?;
                result = result && value;
            }
            Ok(!result)
        }
        other => Err(BaseQueryError::MalformedDefinition {
            path: path.to_owned(),
            violation: format!("filter object key must be and, or, or not, found {other}"),
        }),
    }
}

/// Parses and evaluates one string filter leaf, which may combine
/// individual leaves (`==`, `!=`, `.contains()`, `file.hasTag()`,
/// `file.inFolder()`) with `&&`, unary `!`, `||`, and `(...)` grouping.
/// Obsidian's own
/// documentation states Bases expressions "follow JavaScript behavior";
/// this uses exactly JavaScript's own precedence for these operators (`!`
/// binds tightest, then `&&`, then `||` loosest), rather than inventing a
/// bespoke precedence. A leaf containing no operators (the common case,
/// and the only form previously supported) is handled identically to
/// before: the whole trimmed string passed straight to [`evaluate_leaf`].
///
/// # Errors
///
/// Returns [`BaseQueryError::UnsupportedFilterExpression`] for unbalanced
/// parentheses, or when any leaf substring falls outside the documented
/// grammar (see [`evaluate_leaf`]).
fn evaluate_expression(expr: &str, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    let mut cursor = ExpressionCursor { text: expr, pos: 0 };
    let result = parse_or(&mut cursor, row, path)?;
    cursor.skip_whitespace();
    if cursor.pos != cursor.text.len() {
        return Err(BaseQueryError::UnsupportedFilterExpression {
            path: path.to_owned(),
            expression: expr.trim().to_owned(),
        });
    }
    Ok(result)
}

/// A byte-position cursor into one filter expression string. Every
/// position this cursor ever stops at is the byte offset of an ASCII
/// operator character (`&`, `|`, `!`, `(`, `)`, a quote, or whitespace) or
/// the string's end, all of which are always valid UTF-8 char boundaries,
/// so every slice taken through this cursor is guaranteed valid.
struct ExpressionCursor<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> ExpressionCursor<'a> {
    fn skip_whitespace(&mut self) {
        let bytes = self.text.as_bytes();
        while self.pos < bytes.len() && bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }
}

fn parse_or(cursor: &mut ExpressionCursor<'_>, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    let mut result = parse_and(cursor, row, path)?;
    loop {
        cursor.skip_whitespace();
        if !cursor.rest().starts_with("||") {
            return Ok(result);
        }
        cursor.pos += 2;
        let rhs = parse_and(cursor, row, path)?;
        result = result || rhs;
    }
}

fn parse_and(cursor: &mut ExpressionCursor<'_>, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    let mut result = parse_not(cursor, row, path)?;
    loop {
        cursor.skip_whitespace();
        if !cursor.rest().starts_with("&&") {
            return Ok(result);
        }
        cursor.pos += 2;
        let rhs = parse_not(cursor, row, path)?;
        result = result && rhs;
    }
}

fn parse_not(cursor: &mut ExpressionCursor<'_>, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    cursor.skip_whitespace();
    // `!=` belongs to a comparison leaf, not a NOT prefix: only a bare `!`
    // not immediately followed by `=` is the unary operator.
    if cursor.rest().starts_with('!') && !cursor.rest().starts_with("!=") {
        cursor.pos += 1;
        return Ok(!parse_not(cursor, row, path)?);
    }
    parse_primary(cursor, row, path)
}

fn parse_primary(cursor: &mut ExpressionCursor<'_>, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    cursor.skip_whitespace();
    if cursor.rest().starts_with('(') {
        cursor.pos += 1;
        let result = parse_or(cursor, row, path)?;
        cursor.skip_whitespace();
        if !cursor.rest().starts_with(')') {
            return Err(BaseQueryError::UnsupportedFilterExpression {
                path: path.to_owned(),
                expression: cursor.text.trim().to_owned(),
            });
        }
        cursor.pos += 1;
        return Ok(result);
    }
    let leaf = scan_leaf(cursor);
    evaluate_leaf(leaf.trim(), row, path)
}

/// Scans forward from the cursor's current position to the end of one
/// leaf expression: up to (but not consuming) a top-level `&&`, `||`, or
/// `)`, respecting quoted string literals and the leaf grammar's own
/// balanced parentheses (`.contains(...)`, `file.hasTag(...)`,
/// `file.inFolder(...)`), so a
/// quoted or function-call `&`/`|`/`)` is never mistaken for an operator
/// or a grouping close.
fn scan_leaf<'a>(cursor: &mut ExpressionCursor<'a>) -> &'a str {
    let start = cursor.pos;
    let bytes = cursor.text.as_bytes();
    let mut depth: u32 = 0;
    let mut quote: Option<u8> = None;
    while cursor.pos < bytes.len() {
        let byte = bytes[cursor.pos];
        if let Some(open) = quote {
            cursor.pos += 1;
            if byte == open {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                cursor.pos += 1;
            }
            b'(' => {
                depth += 1;
                cursor.pos += 1;
            }
            b')' if depth == 0 => break,
            b')' => {
                depth -= 1;
                cursor.pos += 1;
            }
            b'&' if depth == 0 && bytes.get(cursor.pos + 1) == Some(&b'&') => break,
            b'|' if depth == 0 && bytes.get(cursor.pos + 1) == Some(&b'|') => break,
            _ => cursor.pos += 1,
        }
    }
    &cursor.text[start..cursor.pos]
}

#[derive(Clone, Copy)]
enum ComparisonOp {
    Equal,
    NotEqual,
}

fn evaluate_leaf(expr: &str, row: &RowContext<'_>, path: &str) -> Result<bool, BaseQueryError> {
    let trimmed = expr.trim();
    if let Some(inner) = trimmed
        .strip_prefix("file.hasTag(")
        .and_then(|rest| rest.strip_suffix(')'))
        && let Some(tag) = parse_quoted(inner.trim())
    {
        return Ok(row.tags.iter().any(|candidate| candidate == tag));
    }
    if let Some(inner) = trimmed
        .strip_prefix("file.inFolder(")
        .and_then(|rest| rest.strip_suffix(')'))
        && let Some(folder) = parse_quoted(inner.trim())
    {
        return Ok(in_folder(row.file.folder, folder));
    }
    if let Some((property, needle)) = parse_contains(trimmed) {
        let value = resolve_property(property, row, path)?;
        return Ok(match value {
            Some(Value::String(text)) => text.contains(needle),
            Some(Value::Array(items)) => items.iter().any(|item| item.as_str() == Some(needle)),
            _ => false,
        });
    }
    if let Some((property, operator, rhs)) = parse_comparison(trimmed) {
        let value = resolve_property(property, row, path)?.unwrap_or(Value::Null);
        let rhs_value = parse_literal(rhs, path)?;
        return Ok(match operator {
            ComparisonOp::Equal => value == rhs_value,
            ComparisonOp::NotEqual => value != rhs_value,
        });
    }
    Err(BaseQueryError::UnsupportedFilterExpression {
        path: path.to_owned(),
        expression: trimmed.to_owned(),
    })
}

/// Obsidian's own `file.inFolder(folder)` semantics: true when the
/// candidate's `file.folder` is exactly `folder` or nested under it
/// (`folder` followed by `/`) — "in folder or subfolder", depth rather than
/// a bare-equality breadth match. Unlike `.contains()`, this is anchored at
/// the start of `candidate_folder`, so [`leaf_scan_hint`] can safely treat
/// it as a scan-root narrowing hint.
fn in_folder(candidate_folder: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    candidate_folder == folder || candidate_folder.starts_with(&format!("{folder}/"))
}

fn parse_contains(expr: &str) -> Option<(&str, &str)> {
    let open = expr.find(".contains(")?;
    let property = expr[..open].trim();
    let rest = &expr[open.saturating_add(".contains(".len())..];
    let inner = rest.strip_suffix(')')?;
    let needle = parse_quoted(inner.trim())?;
    Some((property, needle))
}

fn parse_comparison(expr: &str) -> Option<(&str, ComparisonOp, &str)> {
    if let Some(position) = expr.find("!=") {
        let (lhs, rhs) = expr.split_at(position);
        return Some((lhs.trim(), ComparisonOp::NotEqual, rhs[2..].trim()));
    }
    if let Some(position) = expr.find("==") {
        let (lhs, rhs) = expr.split_at(position);
        return Some((lhs.trim(), ComparisonOp::Equal, rhs[2..].trim()));
    }
    None
}

fn parse_quoted(text: &str) -> Option<&str> {
    let quote = text.chars().next()?;
    if !matches!(quote, '\'' | '"') || text.len() < 2 || !text.ends_with(quote) {
        return None;
    }
    Some(&text[quote.len_utf8()..text.len().saturating_sub(quote.len_utf8())])
}

fn parse_literal(text: &str, path: &str) -> Result<Value, BaseQueryError> {
    if let Some(quoted) = parse_quoted(text) {
        return Ok(Value::String(quoted.to_owned()));
    }
    match text {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        "null" => return Ok(Value::Null),
        _ => {}
    }
    if let Ok(number) = text.parse::<i64>() {
        return Ok(Value::from(number));
    }
    if let Ok(number) = text.parse::<f64>() {
        return Ok(serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number));
    }
    Err(BaseQueryError::UnsupportedFilterExpression {
        path: path.to_owned(),
        expression: text.to_owned(),
    })
}

/// Resolves `property` against `row`: a frontmatter key, optionally
/// prefixed `note.` (Obsidian's own documented equivalence — `note.author`
/// and bare `author` name the same frontmatter key), or any documented
/// `file.*` property (`ext`/`name`/`basename`/`path`/`folder`/`size`/
/// `ctime`/`mtime`/`tags`/`links`/`embeds`/`backlinks`/`properties`).
/// `None` means the frontmatter key is absent (treated as `null` by
/// comparisons); a `formula.*` or genuinely unrecognised `file.*` reference
/// is a hard error, not a `None`.
///
/// # Errors
///
/// See [`BaseQueryError::FormulaReference`] and
/// [`BaseQueryError::UnsupportedFileProperty`]. Exposed publicly for
/// `base_query`'s sort-key resolution (`contextos-mcp`), which needs
/// the same "formula and unrecognised file properties are errors, missing
/// frontmatter is `None`" semantics filter evaluation already applies.
pub fn resolve_property(property: &str, row: &RowContext<'_>, path: &str) -> Result<Option<Value>, BaseQueryError> {
    validate_property_name(property, path)?;
    if let Some(file_property) = property.strip_prefix("file.") {
        let value = match file_property {
            "ext" => Value::String(row.file.ext.to_owned()),
            "name" => Value::String(row.file.name.to_owned()),
            "basename" => Value::String(row.file.basename.to_owned()),
            "path" => Value::String(row.file.path.to_owned()),
            "folder" => Value::String(row.file.folder.to_owned()),
            "size" => Value::from(row.file.size),
            "ctime" => row
                .file
                .ctime
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
            "mtime" => row
                .file
                .mtime
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
            "tags" => string_list(row.tags),
            "links" => string_list(row.links),
            "embeds" => string_list(row.embeds),
            "backlinks" => string_list(row.backlinks),
            "properties" => Value::Object(row.frontmatter.clone()),
            other => {
                return Err(BaseQueryError::UnsupportedFileProperty {
                    path: path.to_owned(),
                    property: other.to_owned(),
                });
            }
        };
        return Ok(Some(value));
    }
    let frontmatter_key = property.strip_prefix("note.").unwrap_or(property);
    Ok(row.frontmatter.get(frontmatter_key).cloned())
}

fn string_list(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

fn validate_property_name(property: &str, path: &str) -> Result<(), BaseQueryError> {
    if let Some(name) = property.strip_prefix("formula.") {
        return Err(BaseQueryError::FormulaReference {
            path: path.to_owned(),
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// One resolved display-column value: the property's JSON value (`Null`
/// when a frontmatter key is absent), a resolved link to the row's own
/// file, or an unevaluated formula marker for any formula body outside the
/// documented subset [`resolve_formula`] evaluates. Unlike a filter or sort
/// key, a display column naming `formula.*` is never an error: an
/// unrecognised formula body still shows the column, carrying the formula
/// name rather than a value, the same graceful-degradation policy an
/// unsupported filter expression does not get (there, failing closed is
/// the whole point).
#[derive(Clone, Debug, PartialEq)]
pub enum ColumnValue {
    Value(Value),
    UnevaluatedFormula(String),
    /// `file.asLink(display?)`: a link to the row's own file
    /// ([`RowContext::file`]'s `path`), with `display` as its label.
    Link {
        target: String,
        display: String,
    },
}

/// Resolves one display column's value for one row. `formulas` is the
/// document's own `formulas` map ([`QueryDefinition::formulas`]), consulted
/// only for a `formula.*` column.
///
/// # Errors
///
/// Returns [`BaseQueryError::UnsupportedFileProperty`] for a `file.*`
/// accessor outside Obsidian's own documented set. A `formula.*` column
/// always succeeds: see [`resolve_formula`] for which formula bodies
/// resolve to a real value versus [`ColumnValue::UnevaluatedFormula`].
pub fn resolve_column(
    column: &str,
    row: &RowContext<'_>,
    path: &str,
    formulas: &Map<String, Value>,
) -> Result<ColumnValue, BaseQueryError> {
    if let Some(name) = column.strip_prefix("formula.") {
        return Ok(resolve_formula(name, formulas, row, path));
    }
    let value = resolve_property(column, row, path)?.unwrap_or(Value::Null);
    Ok(ColumnValue::Value(value))
}

/// Evaluates a documented subset of Obsidian's formula language: a formula
/// body that is (after trimming) a bare property reference (any `file.*`
/// property, or a frontmatter/`note.*` key — the same reference grammar
/// [`resolve_property`] already applies to a filter leaf or column name)
/// resolves to that property's own value, and `file.asLink(display?)`
/// resolves to a [`ColumnValue::Link`] targeting the row's own file, with
/// `display` evaluated the same way (a bare property reference), defaulting
/// to the file's own basename when the argument is omitted or evaluates to
/// no value — Obsidian's own Bases links never render with empty text.
/// Every other formula body (arithmetic, `if()`, date functions, any
/// function this subset does not implement) degrades to
/// [`ColumnValue::UnevaluatedFormula`] rather than guessing at a value or
/// failing the whole query.
fn resolve_formula(name: &str, formulas: &Map<String, Value>, row: &RowContext<'_>, path: &str) -> ColumnValue {
    let unevaluated = || ColumnValue::UnevaluatedFormula(name.to_owned());
    let Some(Value::String(expression)) = formulas.get(name) else {
        return unevaluated();
    };
    let trimmed = expression.trim();
    if let Some(inner) = trimmed
        .strip_prefix("file.asLink(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return match resolve_link_display(inner.trim(), row, path) {
            Some(display) => ColumnValue::Link {
                target: row.file.path.to_owned(),
                display,
            },
            None => unevaluated(),
        };
    }
    if is_bare_property_reference(trimmed) {
        return match resolve_property(trimmed, row, path) {
            Ok(value) => ColumnValue::Value(value.unwrap_or(Value::Null)),
            Err(_) => unevaluated(),
        };
    }
    unevaluated()
}

/// Resolves `file.asLink`'s own `display` argument: empty (no argument)
/// always yields the file's own basename; a bare property reference that
/// resolves to no value (absent or `null`) also falls back to the
/// basename, matching Obsidian's own "a link always shows some text"
/// behaviour; any other argument shape (not a bare property reference, or
/// one this subset cannot resolve) yields `None`, signalling the whole
/// formula is unevaluated.
fn resolve_link_display(argument: &str, row: &RowContext<'_>, path: &str) -> Option<String> {
    if argument.is_empty() {
        return Some(row.file.basename.to_owned());
    }
    if !is_bare_property_reference(argument) {
        return None;
    }
    match resolve_property(argument, row, path) {
        Ok(Some(value)) => Some(scalar_text(&value)),
        Ok(None) => Some(row.file.basename.to_owned()),
        Err(_) => None,
    }
}

/// Whether `expr` is syntactically a bare property reference rather than a
/// larger expression: every documented property/frontmatter-key name is
/// alphanumerics, `_`, `.`, or `-` only, so any other character (an
/// operator, a quote, whitespace, a function call's parentheses) means
/// this is some other formula shape [`resolve_formula`] does not evaluate.
fn is_bare_property_reference(expr: &str) -> bool {
    !expr.is_empty()
        && expr
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Resolves every column for one row, in column order. `formulas` is the
/// document's own `formulas` map, see [`resolve_column`].
///
/// # Errors
///
/// See [`resolve_column`].
pub fn resolve_row(
    columns: &[String],
    row: &RowContext<'_>,
    formulas: &Map<String, Value>,
) -> Result<Vec<ColumnValue>, BaseQueryError> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| resolve_column(column, row, &format!("columns[{index}]"), formulas))
        .collect()
}

/// The single textual representation `base_query` renders: a caller who
/// wants structured JSON asks for [`QueryFormat::Json`] and parses the
/// resulting `content` string itself, rather than the tool always paying to
/// build and ship a second, usually-unwanted representation alongside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryFormat {
    Table,
    Json,
    Csv,
}

/// Renders resolved rows as the requested single textual representation.
#[must_use]
pub fn render(columns: &[String], rows: &[Vec<ColumnValue>], format: QueryFormat) -> String {
    match format {
        QueryFormat::Table => render_table(columns, rows),
        QueryFormat::Json => render_json(columns, rows),
        QueryFormat::Csv => render_csv(columns, rows),
    }
}

fn cell_text(value: &ColumnValue) -> String {
    match value {
        ColumnValue::UnevaluatedFormula(name) => format!("formula.{name} (not evaluated)"),
        ColumnValue::Value(value) => scalar_text(value),
        ColumnValue::Link { target, display } => format!("[{display}]({target})"),
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(scalar_text).collect::<Vec<_>>().join(", "),
        Value::Object(_) => value.to_string(),
    }
}

fn render_table(columns: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let mut out = String::new();
    write_table_row(&mut out, columns.iter().map(|column| escape_table_cell(column)));
    out.push('|');
    for _ in columns {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in rows {
        write_table_row(&mut out, row.iter().map(|value| escape_table_cell(&cell_text(value))));
    }
    out
}

fn write_table_row(out: &mut String, cells: impl Iterator<Item = String>) {
    out.push('|');
    for cell in cells {
        out.push(' ');
        out.push_str(&cell);
        out.push_str(" |");
    }
    out.push('\n');
}

fn escape_table_cell(text: &str) -> String {
    text.replace('\\', "\\\\").replace('|', "\\|").replace('\n', " ")
}

fn render_json(columns: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let array: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut object = Map::new();
            for (column, value) in columns.iter().zip(row.iter()) {
                let json_value = match value {
                    ColumnValue::UnevaluatedFormula(name) => Value::String(format!("formula.{name} (not evaluated)")),
                    ColumnValue::Value(value) => value.clone(),
                    ColumnValue::Link { target, display } => {
                        let mut link = Map::new();
                        link.insert("type".to_owned(), Value::String("link".to_owned()));
                        link.insert("target".to_owned(), Value::String(target.clone()));
                        link.insert("display".to_owned(), Value::String(display.clone()));
                        Value::Object(link)
                    }
                };
                object.insert(column.clone(), json_value);
            }
            Value::Object(object)
        })
        .collect();
    // Defensive only: every `Value` reachable here is built from frontmatter
    // JSON already parsed successfully, or from `parse_literal`'s finite-only
    // number construction (never `NaN`/`Infinity`), so `serde_json`
    // serialisation of this tree cannot actually fail.
    serde_json::to_string_pretty(&Value::Array(array)).unwrap_or_default()
}

fn render_csv(columns: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let mut out = String::new();
    out.push_str(&csv_record(columns.iter().map(String::as_str)));
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = row.iter().map(cell_text).collect();
        out.push_str(&csv_record(cells.iter().map(String::as_str)));
        out.push('\n');
    }
    out
}

fn csv_record<'a>(cells: impl Iterator<Item = &'a str>) -> String {
    cells.map(csv_field).collect::<Vec<_>>().join(",")
}

fn csv_field(field: &str) -> String {
    if field.contains(['"', ',', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

/// Orders two resolved property values for `base_query` sorting. Missing
/// values (`None`) sort before every present value; among present values,
/// `Null < Bool < Number < String`, then within-type ordering (`false <
/// true`; numeric; lexicographic). Arrays and objects have no defined order
/// here (arbitrary but stable, by discriminant only) since sorting on them
/// is out of this iteration's scope.
#[must_use]
pub fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => compare_present(a, b),
    }
}

fn type_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

fn compare_present(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or_default()
            .partial_cmp(&b.as_f64().unwrap_or_default())
            .unwrap_or(Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => type_rank(a).cmp(&type_rank(b)),
    }
}

/// Typed `base_query` execution failures: view/column resolution, filter
/// evaluation, and the documented grammar boundary. Distinct from
/// [`crate::BaseError`], which governs the `.base` YAML document itself.
#[derive(Debug, Error)]
pub enum BaseQueryError {
    #[error("Base query view {name:?} was not found")]
    ViewNotFound { name: String },
    #[error("Base query view {view} has no order or properties to resolve display columns from")]
    NoColumns { view: String },
    #[error("Base query definition is invalid at {path}: {violation}")]
    MalformedDefinition { path: String, violation: String },
    #[error("Base query filter at {path} is not supported: {expression}")]
    UnsupportedFilterExpression { path: String, expression: String },
    #[error(
        "Base query filter at {path} references formula.{name}, which base_query never evaluates in a filter or sort key"
    )]
    FormulaReference { path: String, name: String },
    #[error("Base query filter at {path} references unsupported file property file.{property}")]
    UnsupportedFileProperty { path: String, property: String },
}

impl BaseQueryError {
    /// Returns the stable machine-readable `base_query` error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ViewNotFound { .. } => "query/base-view-not-found",
            Self::NoColumns { .. } => "query/base-query-no-columns",
            Self::MalformedDefinition { .. } => "query/base-query-malformed",
            Self::UnsupportedFilterExpression { .. } => "query/base-query-unsupported-filter",
            Self::FormulaReference { .. } => "query/base-query-formula-reference",
            Self::UnsupportedFileProperty { .. } => "query/base-query-unsupported-file-property",
        }
    }

    /// Returns an actionable correction for the failed query.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::ViewNotFound { .. } => "Pass an existing view name, or omit view to use the first view.",
            Self::NoColumns { .. } => {
                "Add an order list to the view or definition, or properties to the base, before querying it."
            }
            Self::MalformedDefinition { .. } => "Correct the reported definition path and retry.",
            Self::UnsupportedFilterExpression { .. } => {
                "Rewrite the filter using ==, !=, .contains(), file.hasTag(), file.inFolder(), &&, ||, !, or (...) grouping, or remove it."
            }
            Self::FormulaReference { .. } => {
                "base_query does not evaluate formula.* in a filter or sort key; remove the reference or filter/sort on a property instead."
            }
            Self::UnsupportedFileProperty { .. } => {
                "base_query supports file.ext, file.name, file.basename, file.path, file.folder, file.size, file.ctime, file.mtime, file.tags, file.links, file.embeds, file.backlinks, and file.properties; use a frontmatter property for anything else."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BaseQueryError, ColumnValue, FileMetadata, QueryDefinition, QueryFormat, RowContext, ScanRootHint,
        compare_values, evaluate_filters, render, resolve_column, resolve_property, resolve_row, scan_root_hint,
    };
    use crate::BaseDocument;
    use serde_json::{Map, Value, json};

    fn frontmatter(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    fn active_row<'a>(frontmatter: &'a Map<String, Value>, tags: &'a [String]) -> RowContext<'a> {
        RowContext {
            frontmatter,
            file: FileMetadata {
                name: "alpha.md",
                basename: "alpha",
                path: "notes/alpha.md",
                ext: "md",
                folder: "notes",
                size: 42,
                ctime: Some("2024-01-01 00:00:00.0 +00:00:00"),
                mtime: Some("2024-06-01 00:00:00.0 +00:00:00"),
            },
            tags,
            links: &[],
            embeds: &[],
            backlinks: &[],
        }
    }

    #[test]
    fn equality_and_inequality_match_frontmatter_values() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({ "status": "active" }));
        let row = active_row(&frontmatter, &[]);
        assert!(evaluate_filters(Some(&json!("status == \"active\"")), &row)?);
        assert!(!evaluate_filters(Some(&json!("status != \"active\"")), &row)?);
        assert!(!evaluate_filters(Some(&json!("status == \"archived\"")), &row)?);
        Ok(())
    }

    #[test]
    fn a_note_dot_prefix_resolves_the_same_frontmatter_key_as_the_bare_name() -> Result<(), Box<dyn std::error::Error>>
    {
        let frontmatter = frontmatter(json!({ "state": "done" }));
        let row = active_row(&frontmatter, &[]);
        assert!(evaluate_filters(Some(&json!("note.state == \"done\"")), &row)?);
        assert!(!evaluate_filters(Some(&json!("note.state == \"backlog\"")), &row)?);
        assert_eq!(
            resolve_property("note.state", &row, "columns[0]")?,
            resolve_property("state", &row, "columns[0]")?,
        );
        Ok(())
    }

    #[test]
    fn missing_property_compares_as_null() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        assert!(evaluate_filters(Some(&json!("status == null")), &row)?);
        assert!(!evaluate_filters(Some(&json!("status != null")), &row)?);
        Ok(())
    }

    #[test]
    fn contains_matches_a_string_substring_only() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({ "summary": "quarterly review notes" }));
        let row = active_row(&frontmatter, &[]);
        assert!(evaluate_filters(Some(&json!("summary.contains(\"review\")")), &row)?);
        assert!(!evaluate_filters(Some(&json!("summary.contains(\"absent\")")), &row)?);
        Ok(())
    }

    #[test]
    fn has_tag_checks_the_resolved_tag_set() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let tags = vec!["project/alpha".to_owned()];
        let row = active_row(&frontmatter, &tags);
        assert!(evaluate_filters(Some(&json!("file.hasTag(\"project/alpha\")")), &row)?);
        assert!(!evaluate_filters(Some(&json!("file.hasTag(\"archived\")")), &row)?);
        Ok(())
    }

    #[test]
    fn file_ext_name_basename_and_path_are_readable_leaf_properties() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        assert!(evaluate_filters(Some(&json!("file.ext == \"md\"")), &row)?);
        assert!(evaluate_filters(Some(&json!("file.name == \"alpha.md\"")), &row)?);
        assert!(evaluate_filters(Some(&json!("file.basename == \"alpha\"")), &row)?);
        assert!(evaluate_filters(Some(&json!("file.path == \"notes/alpha.md\"")), &row)?);
        Ok(())
    }

    #[test]
    fn file_size_ctime_and_mtime_are_readable_equality_properties() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        assert!(evaluate_filters(Some(&json!("file.size == 42")), &row)?);
        assert!(!evaluate_filters(Some(&json!("file.size == 7")), &row)?);
        assert!(evaluate_filters(
            Some(&json!("file.ctime == \"2024-01-01 00:00:00.0 +00:00:00\"")),
            &row
        )?);
        assert!(evaluate_filters(
            Some(&json!("file.mtime == \"2024-06-01 00:00:00.0 +00:00:00\"")),
            &row
        )?);

        let no_timestamps = RowContext {
            frontmatter: &frontmatter,
            file: FileMetadata {
                ctime: None,
                mtime: None,
                ..row.file
            },
            ..row
        };
        assert!(evaluate_filters(Some(&json!("file.ctime == null")), &no_timestamps)?);
        Ok(())
    }

    #[test]
    fn file_tags_links_embeds_and_backlinks_are_list_properties_with_membership_contains()
    -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let tags = vec!["project/alpha".to_owned()];
        let links = vec!["Target Note".to_owned()];
        let embeds = vec!["diagram.png".to_owned()];
        let backlinks = vec!["notes/referrer.md".to_owned()];
        let row = RowContext {
            frontmatter: &frontmatter,
            tags: &tags,
            links: &links,
            embeds: &embeds,
            backlinks: &backlinks,
            ..active_row(&frontmatter, &[])
        };
        assert!(evaluate_filters(
            Some(&json!("file.tags.contains(\"project/alpha\")")),
            &row
        )?);
        assert!(!evaluate_filters(
            Some(&json!("file.tags.contains(\"archived\")")),
            &row
        )?);
        assert!(evaluate_filters(
            Some(&json!("file.links.contains(\"Target Note\")")),
            &row
        )?);
        assert!(evaluate_filters(
            Some(&json!("file.embeds.contains(\"diagram.png\")")),
            &row
        )?);
        assert!(evaluate_filters(
            Some(&json!("file.backlinks.contains(\"notes/referrer.md\")")),
            &row
        )?);
        Ok(())
    }

    #[test]
    fn file_properties_exposes_the_whole_frontmatter_map_for_display() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({ "status": "active" }));
        let row = active_row(&frontmatter, &[]);
        let resolved = super::resolve_property("file.properties", &row, "columns[0]")?;
        assert_eq!(resolved, Some(json!({ "status": "active" })));
        Ok(())
    }

    #[test]
    fn file_folder_is_a_readable_leaf_property_derived_from_path() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let nested = RowContext {
            frontmatter: &frontmatter,
            file: FileMetadata {
                name: "foo.md",
                basename: "foo",
                path: "memory/tasks/foo.md",
                ext: "md",
                folder: "memory/tasks",
                size: 0,
                ctime: None,
                mtime: None,
            },
            tags: &[],
            links: &[],
            embeds: &[],
            backlinks: &[],
        };
        assert!(evaluate_filters(
            Some(&json!("file.folder == \"memory/tasks\"")),
            &nested
        )?);
        assert!(!evaluate_filters(Some(&json!("file.folder == \"memory\"")), &nested)?);

        let root = RowContext {
            frontmatter: &frontmatter,
            file: FileMetadata {
                name: "root-note.md",
                basename: "root-note",
                path: "root-note.md",
                ext: "md",
                folder: "",
                size: 0,
                ctime: None,
                mtime: None,
            },
            tags: &[],
            links: &[],
            embeds: &[],
            backlinks: &[],
        };
        assert!(evaluate_filters(Some(&json!("file.folder == \"\"")), &root)?);
        assert!(!evaluate_filters(
            Some(&json!("file.folder == \"memory/tasks\"")),
            &root
        )?);
        Ok(())
    }

    #[test]
    fn and_or_not_combine_per_the_obsidian_bases_tree_shape() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({ "status": "active" }));
        let tags = vec!["review".to_owned()];
        let row = active_row(&frontmatter, &tags);
        let filters = json!({
            "and": [
                "file.ext == \"md\"",
                { "or": ["status == \"active\"", { "not": ["file.hasTag(\"archived\")"] }] }
            ]
        });
        assert!(evaluate_filters(Some(&filters), &row)?);

        let excluding = json!({
            "and": [
                "file.ext == \"md\"",
                { "not": ["status == \"active\""] }
            ]
        });
        assert!(!evaluate_filters(Some(&excluding), &row)?);
        Ok(())
    }

    #[test]
    fn string_level_and_or_not_operators_combine_leaves_with_js_precedence() -> Result<(), Box<dyn std::error::Error>> {
        // The real-world shape memory/tasks.base's "Decision pending
        // review" view uses: a single string leaf combining two
        // comparisons with `&&`, previously rejected outright as an
        // unsupported filter expression.
        let decision_frontmatter = frontmatter(json!({ "register": "decision", "completed": null }));
        let decision_row = active_row(&decision_frontmatter, &[]);
        assert!(evaluate_filters(
            Some(&json!("register == \"decision\" && completed == null")),
            &decision_row
        )?);
        assert!(!evaluate_filters(
            Some(&json!("register == \"decision\" && completed == \"2024-01-01\"")),
            &decision_row
        )?);

        // `||` binds looser than `&&`: `a || (b && c)`, not `(a || b) && c`.
        let frontmatter = frontmatter(json!({ "status": "active", "priority": "low" }));
        let row = active_row(&frontmatter, &[]);
        assert!(evaluate_filters(
            Some(&json!(
                "status == \"archived\" || status == \"active\" && priority == \"low\""
            )),
            &row
        )?);
        assert!(!evaluate_filters(
            Some(&json!(
                "status == \"archived\" || status == \"active\" && priority == \"high\""
            )),
            &row
        )?);

        // Explicit `(...)` grouping overrides the default precedence.
        assert!(!evaluate_filters(
            Some(&json!(
                "(status == \"archived\" || status == \"active\") && priority == \"high\""
            )),
            &row
        )?);

        // Unary `!`, and `!=` inside a leaf is never mistaken for it.
        assert!(evaluate_filters(
            Some(&json!("!(status == \"archived\") && priority != \"high\"")),
            &row
        )?);
        Ok(())
    }

    #[test]
    fn unbalanced_parentheses_in_a_string_expression_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        let Err(error) = evaluate_filters(Some(&json!("(status == \"active\"")), &row) else {
            return Err("expected an unclosed group to be rejected".into());
        };
        assert!(matches!(&error, BaseQueryError::UnsupportedFilterExpression { .. }));
        Ok(())
    }

    #[test]
    fn none_filters_matches_every_row() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        assert!(evaluate_filters(None, &row)?);
        Ok(())
    }

    #[test]
    fn a_formula_reference_fails_closed_rather_than_being_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        let Err(error) = evaluate_filters(Some(&json!("formula.display_status != \"\"")), &row) else {
            return Err("expected formula.display_status to be rejected".into());
        };
        assert!(matches!(&error, BaseQueryError::FormulaReference { name, .. } if name == "display_status"));
        assert_eq!(error.code(), "query/base-query-formula-reference");
        Ok(())
    }

    #[test]
    fn an_unsupported_file_property_is_a_named_error_not_a_silent_frontmatter_lookup()
    -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        let Err(error) = evaluate_filters(Some(&json!("file.frobnicate == \"x\"")), &row) else {
            return Err("expected file.frobnicate to be rejected".into());
        };
        assert!(matches!(&error, BaseQueryError::UnsupportedFileProperty { property, .. } if property == "frobnicate"));
        assert_eq!(error.code(), "query/base-query-unsupported-file-property");
        Ok(())
    }

    #[test]
    fn an_expression_outside_the_documented_grammar_is_rejected_naming_the_expression()
    -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({ "price": 12 }));
        let row = active_row(&frontmatter, &[]);
        let Err(error) = evaluate_filters(Some(&json!("price > 10")), &row) else {
            return Err("expected an unsupported comparator to be rejected".into());
        };
        assert!(
            matches!(&error, BaseQueryError::UnsupportedFilterExpression { expression, .. } if expression == "price > 10")
        );
        assert_eq!(error.code(), "query/base-query-unsupported-filter");
        Ok(())
    }

    #[test]
    fn scan_root_hint_finds_a_file_path_equality_leaf_anywhere_in_the_tree() {
        let filters = json!({
            "and": ["file.path == \"notes/alpha.md\"", "status == \"active\""]
        });
        assert_eq!(
            scan_root_hint(Some(&filters)),
            Some(ScanRootHint::Path("notes/alpha.md"))
        );

        let none = json!("status == \"active\"");
        assert_eq!(scan_root_hint(Some(&none)), None);
        assert_eq!(scan_root_hint(None), None);
    }

    #[test]
    fn scan_root_hint_ignores_contains_since_a_substring_can_match_outside_the_directory() {
        let filters = json!("file.path.contains(\"archive\")");
        assert_eq!(scan_root_hint(Some(&filters)), None);
    }

    #[test]
    fn scan_root_hint_finds_a_file_folder_equality_leaf_and_uses_the_value_directly() {
        let filters = json!("file.folder == \"memory/tasks\"");
        assert_eq!(
            scan_root_hint(Some(&filters)),
            Some(ScanRootHint::Folder("memory/tasks"))
        );

        let contains = json!("file.folder.contains(\"tasks\")");
        assert_eq!(scan_root_hint(Some(&contains)), None);
    }

    #[test]
    fn file_in_folder_matches_the_folder_itself_and_any_subfolder_but_not_a_sibling_with_a_shared_prefix()
    -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let exact = RowContext {
            frontmatter: &frontmatter,
            file: FileMetadata {
                folder: "memory/tasks",
                ..active_row(&frontmatter, &[]).file
            },
            ..active_row(&frontmatter, &[])
        };
        assert!(evaluate_filters(
            Some(&json!("file.inFolder(\"memory/tasks\")")),
            &exact
        )?);

        let nested = RowContext {
            file: FileMetadata {
                folder: "memory/tasks/altorum/finance",
                ..exact.file
            },
            ..exact
        };
        assert!(evaluate_filters(
            Some(&json!("file.inFolder(\"memory/tasks\")")),
            &nested
        )?);

        // "memory/tasks2" shares "memory/tasks" as a string prefix but is a
        // sibling folder, not a subfolder: `file.inFolder` must not treat
        // the two as related the way an unanchored `.contains()` would.
        let sibling = RowContext {
            file: FileMetadata {
                folder: "memory/tasks2",
                ..exact.file
            },
            ..exact
        };
        assert!(!evaluate_filters(
            Some(&json!("file.inFolder(\"memory/tasks\")")),
            &sibling
        )?);
        Ok(())
    }

    #[test]
    fn scan_root_hint_recognises_file_in_folder_as_a_safe_narrowing_hint() {
        let filters = json!("file.inFolder(\"memory/tasks\")");
        assert_eq!(
            scan_root_hint(Some(&filters)),
            Some(ScanRootHint::Folder("memory/tasks"))
        );
    }

    #[test]
    fn scan_root_hint_never_narrows_through_or_unless_every_operand_agrees() {
        // The real bug this guards: an `or` where only one operand happens
        // to carry a `file.folder`/`file.path` equality leaf must not
        // narrow the scan, since a row could satisfy the `or` through the
        // *other* operand, entirely outside that leaf's directory.
        let disagreeing = json!({
            "or": ["file.folder == \"a\"", "status == \"active\""]
        });
        assert_eq!(scan_root_hint(Some(&disagreeing)), None);

        // Every operand agreeing on the identical hint is still safe.
        let agreeing = json!({
            "or": ["file.folder == \"a\"", "file.folder == \"a\""]
        });
        assert_eq!(scan_root_hint(Some(&agreeing)), Some(ScanRootHint::Folder("a")));
    }

    #[test]
    fn scan_root_hint_never_narrows_through_not() {
        // A hint on the negated operand describes where matches *of that
        // operand* live, not matches of its negation: narrowing here would
        // scan only "a" when the real matches are everywhere outside it.
        let filters = json!({ "not": ["file.folder == \"a\""] });
        assert_eq!(scan_root_hint(Some(&filters)), None);
    }

    #[test]
    fn scan_root_hint_parses_compound_string_expressions_with_the_real_grammar() {
        // `&&`: either conjunct narrowing is safe, regardless of position.
        assert_eq!(
            scan_root_hint(Some(&json!("file.basename != \"index\" && file.folder == \"a\""))),
            Some(ScanRootHint::Folder("a"))
        );

        // The exact real-world shape memory/tasks.base uses: a `||` whose
        // second operand is an unanchored `.contains()` must not narrow,
        // even combined with an outer `&&` and parentheses, and even though
        // a naive whole-string `==`/`!=` split would previously have
        // mis-parsed this compound expression entirely.
        let unsafe_shape = json!(
            "(file.folder == \"memory/tasks\" || file.folder.contains(\"memory/tasks/\")) && file.basename != \"index\""
        );
        assert_eq!(scan_root_hint(Some(&unsafe_shape)), None);

        // The `file.inFolder`-based rewrite of that same intent narrows
        // correctly: it is a single anchored leaf, not an `or`.
        let rewritten = json!("file.inFolder(\"memory/tasks\") && file.basename != \"index\"");
        assert_eq!(
            scan_root_hint(Some(&rewritten)),
            Some(ScanRootHint::Folder("memory/tasks"))
        );

        // `!`-negated leaves never narrow.
        assert_eq!(
            scan_root_hint(Some(&json!("!(file.folder == \"a\") && status == \"active\""))),
            None
        );
    }

    #[test]
    fn query_definition_resolves_the_named_view_or_defaults_to_the_first() -> Result<(), Box<dyn std::error::Error>> {
        let source = "views:\n  - type: table\n    name: First\n    order: [file.name]\n  - type: table\n    name: Second\n    order: [status]\n";
        let document = BaseDocument::try_from(source)?;

        let first = QueryDefinition::from_document(&document, None)?;
        assert_eq!(first.columns, ["file.name"]);

        let second = QueryDefinition::from_document(&document, Some("Second"))?;
        assert_eq!(second.columns, ["status"]);

        let Err(error) = QueryDefinition::from_document(&document, Some("Missing")) else {
            return Err("expected an unknown view name to be rejected".into());
        };
        assert!(matches!(&error, BaseQueryError::ViewNotFound { name } if name == "Missing"));
        assert_eq!(error.code(), "query/base-view-not-found");
        Ok(())
    }

    #[test]
    fn query_definition_rejects_a_view_with_neither_order_nor_properties() -> Result<(), Box<dyn std::error::Error>> {
        let source = "views:\n  - type: table\n    name: Bare\n";
        let document = BaseDocument::try_from(source)?;

        let Err(error) = QueryDefinition::from_document(&document, None) else {
            return Err("expected a columnless view to be rejected".into());
        };
        assert!(matches!(&error, BaseQueryError::NoColumns { view } if view == "Bare"));
        assert_eq!(error.code(), "query/base-query-no-columns");
        Ok(())
    }

    #[test]
    fn query_definition_from_inline_reads_the_flat_shape_without_a_views_wrapper()
    -> Result<(), Box<dyn std::error::Error>> {
        let definition = frontmatter(json!({
            "filters": "status == \"active\"",
            "order": ["file.name", "status"],
            "sort": [{ "property": "file.name", "direction": "DESC" }],
            "limit": 5
        }));

        let resolved = QueryDefinition::from_inline(&definition)?;
        assert_eq!(resolved.columns, ["file.name", "status"]);
        assert_eq!(resolved.sort[0].property, "file.name");
        assert!(resolved.sort[0].descending);
        assert_eq!(resolved.limit, Some(5));
        assert!(resolved.filters.is_some());
        Ok(())
    }

    #[test]
    fn compare_values_orders_missing_then_by_type_then_by_value() {
        assert_eq!(compare_values(None, Some(&json!("a"))), std::cmp::Ordering::Less);
        assert_eq!(
            compare_values(Some(&json!(1)), Some(&json!("a"))),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_values(Some(&json!(1)), Some(&json!(2))),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_values(Some(&json!("a")), Some(&json!("b"))),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn a_formula_display_column_with_no_matching_formula_definition_is_an_unevaluated_marker_not_an_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        let value = resolve_column("formula.display_status", &row, "columns[0]", &Map::new())?;
        assert_eq!(value, ColumnValue::UnevaluatedFormula("display_status".to_owned()));
        Ok(())
    }

    #[test]
    fn a_formula_body_outside_the_documented_subset_is_an_unevaluated_marker_not_an_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let formulas = frontmatter(json!({ "days_old": "(now() - file.ctime).days" }));
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        let value = resolve_column("formula.days_old", &row, "columns[0]", &formulas)?;
        assert_eq!(value, ColumnValue::UnevaluatedFormula("days_old".to_owned()));
        Ok(())
    }

    #[test]
    fn a_bare_property_reference_formula_resolves_to_that_propertys_own_value() -> Result<(), Box<dyn std::error::Error>>
    {
        let formulas = frontmatter(json!({ "priority_display": "priority", "path_display": "file.path" }));
        let frontmatter = frontmatter(json!({ "priority": "high" }));
        let row = active_row(&frontmatter, &[]);
        assert_eq!(
            resolve_column("formula.priority_display", &row, "columns[0]", &formulas)?,
            ColumnValue::Value(json!("high"))
        );
        assert_eq!(
            resolve_column("formula.path_display", &row, "columns[0]", &formulas)?,
            ColumnValue::Value(json!("notes/alpha.md"))
        );
        Ok(())
    }

    #[test]
    fn file_as_link_resolves_to_a_link_targeting_the_rows_own_file() -> Result<(), Box<dyn std::error::Error>> {
        let formulas = frontmatter(json!({
            "id_link": "file.asLink(note.id)",
            "bare_link": "file.asLink()",
        }));
        let frontmatter = frontmatter(json!({ "id": "a-042" }));
        let row = active_row(&frontmatter, &[]);
        assert_eq!(
            resolve_column("formula.id_link", &row, "columns[0]", &formulas)?,
            ColumnValue::Link {
                target: "notes/alpha.md".to_owned(),
                display: "a-042".to_owned(),
            }
        );
        // No argument: falls back to the file's own basename.
        assert_eq!(
            resolve_column("formula.bare_link", &row, "columns[0]", &formulas)?,
            ColumnValue::Link {
                target: "notes/alpha.md".to_owned(),
                display: "alpha".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn file_as_link_falls_back_to_the_basename_when_its_display_property_is_absent()
    -> Result<(), Box<dyn std::error::Error>> {
        let formulas = frontmatter(json!({ "id_link": "file.asLink(note.id)" }));
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        assert_eq!(
            resolve_column("formula.id_link", &row, "columns[0]", &formulas)?,
            ColumnValue::Link {
                target: "notes/alpha.md".to_owned(),
                display: "alpha".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn file_as_link_with_an_argument_outside_the_documented_subset_is_an_unevaluated_marker()
    -> Result<(), Box<dyn std::error::Error>> {
        let formulas = frontmatter(json!({ "title_link": "file.asLink(note.title + \"!\")" }));
        let frontmatter = frontmatter(json!({}));
        let row = active_row(&frontmatter, &[]);
        assert_eq!(
            resolve_column("formula.title_link", &row, "columns[0]", &formulas)?,
            ColumnValue::UnevaluatedFormula("title_link".to_owned())
        );
        Ok(())
    }

    #[test]
    fn resolve_row_resolves_frontmatter_file_and_formula_columns_in_order() -> Result<(), Box<dyn std::error::Error>> {
        let formulas = frontmatter(json!({ "title_link": "file.asLink()" }));
        let frontmatter = frontmatter(json!({ "status": "active" }));
        let row = active_row(&frontmatter, &[]);
        let columns = [
            "file.name".to_owned(),
            "status".to_owned(),
            "formula.display_status".to_owned(),
            "formula.title_link".to_owned(),
            "missing".to_owned(),
        ];
        let resolved = resolve_row(&columns, &row, &formulas)?;
        assert_eq!(resolved[0], ColumnValue::Value(json!("alpha.md")));
        assert_eq!(resolved[1], ColumnValue::Value(json!("active")));
        assert_eq!(
            resolved[2],
            ColumnValue::UnevaluatedFormula("display_status".to_owned())
        );
        assert_eq!(
            resolved[3],
            ColumnValue::Link {
                target: "notes/alpha.md".to_owned(),
                display: "alpha".to_owned(),
            }
        );
        assert_eq!(resolved[4], ColumnValue::Value(Value::Null));
        Ok(())
    }

    #[test]
    fn render_table_escapes_pipes_marks_unevaluated_formulas_and_renders_links_as_markdown() {
        let columns = [
            "file.name".to_owned(),
            "formula.display_status".to_owned(),
            "formula.id_link".to_owned(),
        ];
        let rows = vec![vec![
            ColumnValue::Value(json!("a | b")),
            ColumnValue::UnevaluatedFormula("display_status".to_owned()),
            ColumnValue::Link {
                target: "notes/alpha.md".to_owned(),
                display: "a-042".to_owned(),
            },
        ]];
        let table = render(&columns, &rows, QueryFormat::Table);
        assert_eq!(
            table,
            "| file.name | formula.display_status | formula.id_link |\n| --- | --- | --- |\n\
             | a \\| b | formula.display_status (not evaluated) | [a-042](notes/alpha.md) |\n"
        );
    }

    #[test]
    fn render_json_produces_one_object_per_row_keyed_by_column() -> Result<(), Box<dyn std::error::Error>> {
        let columns = ["status".to_owned()];
        let rows = vec![vec![ColumnValue::Value(json!("active"))]];
        let content = render(&columns, &rows, QueryFormat::Json);
        let parsed: Value = serde_json::from_str(&content)?;
        assert_eq!(parsed, json!([{ "status": "active" }]));
        Ok(())
    }

    #[test]
    fn render_json_represents_a_link_column_as_a_typed_object() -> Result<(), Box<dyn std::error::Error>> {
        let columns = ["formula.id_link".to_owned()];
        let rows = vec![vec![ColumnValue::Link {
            target: "notes/alpha.md".to_owned(),
            display: "a-042".to_owned(),
        }]];
        let content = render(&columns, &rows, QueryFormat::Json);
        let parsed: Value = serde_json::from_str(&content)?;
        assert_eq!(
            parsed,
            json!([{ "formula.id_link": { "type": "link", "target": "notes/alpha.md", "display": "a-042" } }])
        );
        Ok(())
    }

    #[test]
    fn render_csv_quotes_fields_containing_commas_or_quotes() {
        let columns = ["title".to_owned()];
        let rows = vec![vec![ColumnValue::Value(json!("hello, \"world\""))]];
        let csv = render(&columns, &rows, QueryFormat::Csv);
        assert_eq!(csv, "title\n\"hello, \"\"world\"\"\"\n");
    }
}
