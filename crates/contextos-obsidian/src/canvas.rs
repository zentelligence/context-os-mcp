use std::collections::BTreeSet;

use contextos_core::ContentHash;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

const GROUP_PADDING: i64 = 40;
const NODE_SPACING: i64 = 60;

/// Input used to create one complete JSON Canvas document.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasCreateInput {
    pub nodes: Vec<Map<String, Value>>,
    pub edges: Vec<Map<String, Value>>,
}

/// One actionable JSON Canvas schema diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanvasDiagnostic {
    pub code: String,
    pub path: String,
    pub message: String,
}

/// One transactional JSON Canvas operation.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasOperation {
    AddNode {
        node: Map<String, Value>,
    },
    UpdateNode {
        id: String,
        patch: Map<String, Value>,
    },
    RemoveNode {
        id: String,
    },
    AddEdge {
        edge: Map<String, Value>,
    },
    UpdateEdge {
        id: String,
        patch: Map<String, Value>,
    },
    RemoveEdge {
        id: String,
    },
    Group {
        group: Map<String, Value>,
        members: Vec<String>,
    },
}

/// A JSON Canvas 1.0 document preserving node, edge, and extension order.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasDocument {
    nodes: Vec<Value>,
    edges: Vec<Value>,
    extensions: Map<String, Value>,
}

impl TryFrom<&str> for CanvasDocument {
    type Error = CanvasError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let mut definition =
            serde_json::from_str::<Map<String, Value>>(source).map_err(|source| CanvasError::InvalidJson {
                line: source.line(),
                column: source.column(),
                source,
            })?;
        let nodes = definition
            .shift_remove("nodes")
            .unwrap_or_else(|| Value::Array(Vec::new()))
            .as_array()
            .cloned()
            .ok_or_else(|| CanvasError::Schema {
                path: "nodes".to_owned(),
                violation: "nodes must be an array".to_owned(),
            })?;
        let edges = definition
            .shift_remove("edges")
            .unwrap_or_else(|| Value::Array(Vec::new()))
            .as_array()
            .cloned()
            .ok_or_else(|| CanvasError::Schema {
                path: "edges".to_owned(),
                violation: "edges must be an array".to_owned(),
            })?;
        Ok(Self {
            nodes,
            edges,
            extensions: definition,
        })
    }
}

impl TryFrom<String> for CanvasDocument {
    type Error = CanvasError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl TryFrom<CanvasCreateInput> for CanvasDocument {
    type Error = CanvasError;

    fn try_from(value: CanvasCreateInput) -> Result<Self, Self::Error> {
        let mut document = Self {
            nodes: Vec::with_capacity(value.nodes.len()),
            edges: Vec::with_capacity(value.edges.len()),
            extensions: Map::new(),
        };
        for (index, mut node) in value.nodes.into_iter().enumerate() {
            ensure_generated_id(&mut node, "node", index, &document.identifiers())?;
            document.nodes.push(Value::Object(node));
        }
        for (index, mut edge) in value.edges.into_iter().enumerate() {
            ensure_generated_id(&mut edge, "edge", index, &document.identifiers())?;
            document.edges.push(Value::Object(edge));
        }
        document.ensure_valid()?;
        Ok(document)
    }
}

impl TryFrom<&CanvasDocument> for String {
    type Error = CanvasError;

    fn try_from(value: &CanvasDocument) -> Result<Self, Self::Error> {
        value.ensure_valid()?;
        let mut definition = value.extensions.clone();
        definition.insert("nodes".to_owned(), Value::Array(value.nodes.clone()));
        definition.insert("edges".to_owned(), Value::Array(value.edges.clone()));
        let mut rendered =
            serde_json::to_string_pretty(&definition).map_err(|source| CanvasError::Serialise { source })?;
        rendered.push('\n');
        Ok(rendered)
    }
}

impl CanvasDocument {
    /// Returns nodes in their z-index order.
    #[must_use]
    pub fn nodes(&self) -> &[Value] {
        &self.nodes
    }

    /// Returns edges in their persisted order.
    #[must_use]
    pub fn edges(&self) -> &[Value] {
        &self.edges
    }

    /// Reports every JSON Canvas 1.0 schema and reference violation.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<CanvasDiagnostic> {
        let mut diagnostics = Vec::new();
        let mut node_ids = BTreeSet::new();
        let mut all_ids = BTreeSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let path = format!("nodes[{index}]");
            validate_node(node, &path, &mut diagnostics);
            if let Some(id) = node.as_object().and_then(|node| node.get("id")).and_then(Value::as_str) {
                node_ids.insert(id);
                if !all_ids.insert(id) {
                    push_diagnostic(
                        &mut diagnostics,
                        "canvas/duplicate-id",
                        &format!("{path}.id"),
                        &format!("ID {id} is duplicated"),
                    );
                }
            }
        }
        for (index, edge) in self.edges.iter().enumerate() {
            let path = format!("edges[{index}]");
            validate_edge(edge, &path, &mut diagnostics);
            let Some(edge) = edge.as_object() else {
                continue;
            };
            if let Some(id) = edge.get("id").and_then(Value::as_str)
                && !all_ids.insert(id)
            {
                push_diagnostic(
                    &mut diagnostics,
                    "canvas/duplicate-id",
                    &format!("{path}.id"),
                    &format!("ID {id} is duplicated"),
                );
            }
            for endpoint in ["fromNode", "toNode"] {
                if let Some(id) = edge.get(endpoint).and_then(Value::as_str)
                    && !node_ids.contains(id)
                {
                    push_diagnostic(
                        &mut diagnostics,
                        "canvas/dangling-edge",
                        &format!("{path}.{endpoint}"),
                        &format!("edge endpoint {id} does not reference a node"),
                    );
                }
            }
        }
        diagnostics
    }

    /// Applies an operation list atomically and validates the complete result.
    ///
    /// # Errors
    ///
    /// Returns a specific operation or final-schema violation without changing
    /// the original canvas.
    pub fn apply(&mut self, operations: Vec<CanvasOperation>) -> Result<(), CanvasError> {
        let mut candidate = self.clone();
        for (index, operation) in operations.into_iter().enumerate() {
            candidate.apply_operation(operation, index)?;
        }
        candidate.ensure_valid()?;
        *self = candidate;
        Ok(())
    }

    fn apply_operation(&mut self, operation: CanvasOperation, index: usize) -> Result<(), CanvasError> {
        match operation {
            CanvasOperation::AddNode { mut node } => {
                let has_x = node.contains_key("x");
                let has_y = node.contains_key("y");
                if !has_x {
                    let x = self
                        .nodes
                        .iter()
                        .filter_map(Value::as_object)
                        .filter_map(|node| NodeBounds::try_from(node).ok())
                        .map(|bounds| bounds.right)
                        .max()
                        .map_or(0, |right| right.saturating_add(NODE_SPACING));
                    node.insert("x".to_owned(), Value::from(x));
                }
                if !has_y {
                    node.insert("y".to_owned(), Value::from(0));
                }
                ensure_generated_id(&mut node, "node", index, &self.identifiers())?;
                self.nodes.push(Value::Object(node));
            }
            CanvasOperation::UpdateNode { id, patch } => {
                let node = object_by_id_mut(&mut self.nodes, &id)
                    .ok_or_else(|| CanvasError::from((index, format!("nodes.{id}"), "node does not exist")))?;
                if patch.contains_key("id") {
                    return Err(CanvasError::from((
                        index,
                        format!("nodes.{id}.id"),
                        "node ID cannot be changed",
                    )));
                }
                merge_patch(node, patch);
                node.insert("id".to_owned(), Value::String(id));
            }
            CanvasOperation::RemoveNode { id } => {
                let Some(position) = value_index_by_id(&self.nodes, &id) else {
                    return Err(CanvasError::from((index, format!("nodes.{id}"), "node does not exist")));
                };
                self.nodes.remove(position);
                self.edges.retain(|edge| {
                    edge.as_object().is_none_or(|edge| {
                        edge.get("fromNode").and_then(Value::as_str) != Some(id.as_str())
                            && edge.get("toNode").and_then(Value::as_str) != Some(id.as_str())
                    })
                });
            }
            CanvasOperation::AddEdge { mut edge } => {
                ensure_generated_id(&mut edge, "edge", index, &self.identifiers())?;
                self.edges.push(Value::Object(edge));
            }
            CanvasOperation::UpdateEdge { id, patch } => {
                let edge = object_by_id_mut(&mut self.edges, &id)
                    .ok_or_else(|| CanvasError::from((index, format!("edges.{id}"), "edge does not exist")))?;
                if patch.contains_key("id") {
                    return Err(CanvasError::from((
                        index,
                        format!("edges.{id}.id"),
                        "edge ID cannot be changed",
                    )));
                }
                merge_patch(edge, patch);
                edge.insert("id".to_owned(), Value::String(id));
            }
            CanvasOperation::RemoveEdge { id } => {
                let Some(position) = value_index_by_id(&self.edges, &id) else {
                    return Err(CanvasError::from((index, format!("edges.{id}"), "edge does not exist")));
                };
                self.edges.remove(position);
            }
            CanvasOperation::Group { mut group, members } => {
                self.group(&mut group, &members, index)?;
            }
        }
        Ok(())
    }

    fn group(&mut self, group: &mut Map<String, Value>, members: &[String], index: usize) -> Result<(), CanvasError> {
        if members.is_empty() {
            return Err(CanvasError::from((
                index,
                "members".to_owned(),
                "group must contain at least one member",
            )));
        }
        let mut unique = BTreeSet::new();
        let mut bounds = Vec::with_capacity(members.len());
        let mut insertion = self.nodes.len();
        for id in members {
            if !unique.insert(id.as_str()) {
                return Err(CanvasError::from((
                    index,
                    "members".to_owned(),
                    "group member IDs must be unique",
                )));
            }
            let Some(position) = value_index_by_id(&self.nodes, id) else {
                return Err(CanvasError::from((
                    index,
                    format!("members.{id}"),
                    "group member does not exist",
                )));
            };
            insertion = insertion.min(position);
            let Some(node) = self.nodes.get(position).and_then(Value::as_object) else {
                return Err(CanvasError::from((
                    index,
                    format!("nodes.{id}"),
                    "group member must be an object",
                )));
            };
            bounds.push(NodeBounds::try_from(node).map_err(|CanvasBoundsError| {
                CanvasError::from((index, format!("nodes.{id}"), "group member must have integer geometry"))
            })?);
        }
        let left = bounds.iter().map(|bounds| bounds.left).min().unwrap_or(0);
        let top = bounds.iter().map(|bounds| bounds.top).min().unwrap_or(0);
        let right = bounds.iter().map(|bounds| bounds.right).max().unwrap_or(0);
        let bottom = bounds.iter().map(|bounds| bounds.bottom).max().unwrap_or(0);
        group.insert("type".to_owned(), Value::String("group".to_owned()));
        group.insert("x".to_owned(), Value::from(left.saturating_sub(GROUP_PADDING)));
        group.insert("y".to_owned(), Value::from(top.saturating_sub(GROUP_PADDING)));
        group.insert(
            "width".to_owned(),
            Value::from(
                right
                    .saturating_sub(left)
                    .saturating_add(GROUP_PADDING.saturating_mul(2)),
            ),
        );
        group.insert(
            "height".to_owned(),
            Value::from(
                bottom
                    .saturating_sub(top)
                    .saturating_add(GROUP_PADDING.saturating_mul(2)),
            ),
        );
        ensure_generated_id(group, "group", index, &self.identifiers())?;
        self.nodes.insert(insertion, Value::Object(group.clone()));
        Ok(())
    }

    fn identifiers(&self) -> BTreeSet<&str> {
        self.nodes
            .iter()
            .chain(&self.edges)
            .filter_map(Value::as_object)
            .filter_map(|value| value.get("id"))
            .filter_map(Value::as_str)
            .collect()
    }

    fn ensure_valid(&self) -> Result<(), CanvasError> {
        match self.diagnostics().into_iter().next() {
            Some(diagnostic) => Err(CanvasError::Schema {
                path: diagnostic.path,
                violation: diagnostic.message,
            }),
            None => Ok(()),
        }
    }
}

fn validate_node(value: &Value, path: &str, diagnostics: &mut Vec<CanvasDiagnostic>) {
    let Some(node) = value.as_object() else {
        push_diagnostic(diagnostics, "canvas/schema", path, "node must be an object");
        return;
    };
    validate_non_empty_string(node.get("id"), &format!("{path}.id"), diagnostics);
    for field in ["x", "y"] {
        validate_integer(node.get(field), &format!("{path}.{field}"), false, diagnostics);
    }
    for field in ["width", "height"] {
        validate_integer(node.get(field), &format!("{path}.{field}"), true, diagnostics);
    }
    validate_color(node.get("color"), &format!("{path}.color"), diagnostics);
    match node.get("type").and_then(Value::as_str) {
        Some("text") => {
            validate_string(node.get("text"), &format!("{path}.text"), diagnostics);
        }
        Some("file") => {
            validate_non_empty_string(node.get("file"), &format!("{path}.file"), diagnostics);
            if node
                .get("subpath")
                .is_some_and(|subpath| !subpath.as_str().is_some_and(|value| value.starts_with('#')))
            {
                push_diagnostic(
                    diagnostics,
                    "canvas/schema",
                    &format!("{path}.subpath"),
                    "file subpath must be a string beginning with #",
                );
            }
        }
        Some("link") => {
            validate_non_empty_string(node.get("url"), &format!("{path}.url"), diagnostics);
        }
        Some("group") => {
            for field in ["label", "background"] {
                if node.get(field).is_some_and(|value| !value.is_string()) {
                    push_diagnostic(
                        diagnostics,
                        "canvas/schema",
                        &format!("{path}.{field}"),
                        &format!("{field} must be a string"),
                    );
                }
            }
            if node
                .get("backgroundStyle")
                .is_some_and(|style| !matches!(style.as_str(), Some("cover" | "ratio" | "repeat")))
            {
                push_diagnostic(
                    diagnostics,
                    "canvas/schema",
                    &format!("{path}.backgroundStyle"),
                    "backgroundStyle must be cover, ratio, or repeat",
                );
            }
        }
        Some(_) => push_diagnostic(
            diagnostics,
            "canvas/schema",
            &format!("{path}.type"),
            "node type must be text, file, link, or group",
        ),
        None => push_diagnostic(
            diagnostics,
            "canvas/schema",
            &format!("{path}.type"),
            "node type must be a string",
        ),
    }
}

fn validate_edge(value: &Value, path: &str, diagnostics: &mut Vec<CanvasDiagnostic>) {
    let Some(edge) = value.as_object() else {
        push_diagnostic(diagnostics, "canvas/schema", path, "edge must be an object");
        return;
    };
    for field in ["id", "fromNode", "toNode"] {
        validate_non_empty_string(edge.get(field), &format!("{path}.{field}"), diagnostics);
    }
    for field in ["fromSide", "toSide"] {
        if edge
            .get(field)
            .is_some_and(|side| !matches!(side.as_str(), Some("top" | "right" | "bottom" | "left")))
        {
            push_diagnostic(
                diagnostics,
                "canvas/schema",
                &format!("{path}.{field}"),
                &format!("{field} must be top, right, bottom, or left"),
            );
        }
    }
    for field in ["fromEnd", "toEnd"] {
        if edge
            .get(field)
            .is_some_and(|end| !matches!(end.as_str(), Some("none" | "arrow")))
        {
            push_diagnostic(
                diagnostics,
                "canvas/schema",
                &format!("{path}.{field}"),
                &format!("{field} must be none or arrow"),
            );
        }
    }
    if edge.get("label").is_some_and(|label| !label.is_string()) {
        push_diagnostic(
            diagnostics,
            "canvas/schema",
            &format!("{path}.label"),
            "edge label must be a string",
        );
    }
    validate_color(edge.get("color"), &format!("{path}.color"), diagnostics);
}

fn validate_string(value: Option<&Value>, path: &str, diagnostics: &mut Vec<CanvasDiagnostic>) {
    if !value.is_some_and(Value::is_string) {
        push_diagnostic(diagnostics, "canvas/schema", path, "value must be a string");
    }
}

fn validate_non_empty_string(value: Option<&Value>, path: &str, diagnostics: &mut Vec<CanvasDiagnostic>) {
    if value.and_then(Value::as_str).is_none_or(str::is_empty) {
        push_diagnostic(diagnostics, "canvas/schema", path, "value must be a non-empty string");
    }
}

fn validate_integer(value: Option<&Value>, path: &str, positive: bool, diagnostics: &mut Vec<CanvasDiagnostic>) {
    if !value
        .and_then(Value::as_i64)
        .is_some_and(|value| !positive || value > 0)
    {
        push_diagnostic(
            diagnostics,
            "canvas/schema",
            path,
            if positive {
                "value must be a positive integer"
            } else {
                "value must be an integer"
            },
        );
    }
}

fn validate_color(value: Option<&Value>, path: &str, diagnostics: &mut Vec<CanvasDiagnostic>) {
    let Some(value) = value else {
        return;
    };
    let valid = value.as_str().is_some_and(|colour| {
        matches!(colour, "1" | "2" | "3" | "4" | "5" | "6")
            || colour
                .strip_prefix('#')
                .is_some_and(|hex| hex.len() == 6 && hex.chars().all(|character| character.is_ascii_hexdigit()))
    });
    if !valid {
        push_diagnostic(
            diagnostics,
            "canvas/schema",
            path,
            "colour must be preset 1–6 or a six-digit hexadecimal string",
        );
    }
}

fn push_diagnostic(diagnostics: &mut Vec<CanvasDiagnostic>, code: &str, path: &str, message: &str) {
    diagnostics.push(CanvasDiagnostic {
        code: code.to_owned(),
        path: path.to_owned(),
        message: message.to_owned(),
    });
}

fn ensure_generated_id(
    object: &mut Map<String, Value>,
    namespace: &str,
    ordinal: usize,
    existing: &BTreeSet<&str>,
) -> Result<(), CanvasError> {
    if object.contains_key("id") {
        return Ok(());
    }
    let mut nonce = 0_u64;
    loop {
        let id = CanvasId::try_from(CanvasIdInput {
            object,
            namespace,
            ordinal,
            nonce,
        })?;
        if !existing.contains(<&str>::from(&id)) {
            object.insert("id".to_owned(), Value::String(String::from(id)));
            return Ok(());
        }
        nonce = nonce.checked_add(1).ok_or(CanvasError::IdentifierGeneration)?;
    }
}

struct CanvasId(String);

struct CanvasIdInput<'a> {
    object: &'a Map<String, Value>,
    namespace: &'a str,
    ordinal: usize,
    nonce: u64,
}

impl TryFrom<CanvasIdInput<'_>> for CanvasId {
    type Error = CanvasError;

    fn try_from(value: CanvasIdInput<'_>) -> Result<Self, Self::Error> {
        let bytes = serde_json::to_vec(value.object).map_err(|source| CanvasError::Serialise { source })?;
        let mut digest = Sha256::new();
        digest.update(value.namespace.as_bytes());
        digest.update(value.ordinal.to_le_bytes());
        digest.update(value.nonce.to_le_bytes());
        digest.update(bytes);
        let hash = ContentHash::from(<[u8; 32]>::from(digest.finalize()));
        let id = <&str>::from(&hash)
            .get(..16)
            .ok_or(CanvasError::IdentifierGeneration)?
            .to_owned();
        Ok(Self(id))
    }
}

impl From<CanvasId> for String {
    fn from(value: CanvasId) -> Self {
        value.0
    }
}

impl<'a> From<&'a CanvasId> for &'a str {
    fn from(value: &'a CanvasId) -> Self {
        &value.0
    }
}

fn value_index_by_id(values: &[Value], id: &str) -> Option<usize> {
    values.iter().position(|value| {
        value
            .as_object()
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            == Some(id)
    })
}

fn object_by_id_mut<'a>(values: &'a mut [Value], id: &str) -> Option<&'a mut Map<String, Value>> {
    values.iter_mut().find_map(|value| {
        let object = value.as_object_mut()?;
        (object.get("id").and_then(Value::as_str) == Some(id)).then_some(object)
    })
}

struct NodeBounds {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

struct CanvasBoundsError;

impl TryFrom<&Map<String, Value>> for NodeBounds {
    type Error = CanvasBoundsError;

    fn try_from(node: &Map<String, Value>) -> Result<Self, Self::Error> {
        let left = node.get("x").and_then(Value::as_i64).ok_or(CanvasBoundsError)?;
        let top = node.get("y").and_then(Value::as_i64).ok_or(CanvasBoundsError)?;
        let width = node.get("width").and_then(Value::as_i64).ok_or(CanvasBoundsError)?;
        let height = node.get("height").and_then(Value::as_i64).ok_or(CanvasBoundsError)?;
        Ok(Self {
            left,
            top,
            right: left.saturating_add(width),
            bottom: top.saturating_add(height),
        })
    }
}

fn merge_patch(target: &mut Map<String, Value>, patch: Map<String, Value>) {
    for (key, value) in patch {
        match value {
            Value::Null => {
                target.shift_remove(&key);
            }
            Value::Object(nested_patch) => {
                let nested = target.entry(key).or_insert_with(|| Value::Object(Map::new()));
                if !nested.is_object() {
                    *nested = Value::Object(Map::new());
                }
                if let Some(nested) = nested.as_object_mut() {
                    merge_patch(nested, nested_patch);
                }
            }
            replacement => {
                target.insert(key, replacement);
            }
        }
    }
}

/// Typed JSON Canvas codec and operation failures.
#[derive(Debug, Error)]
pub enum CanvasError {
    #[error("JSON Canvas is invalid JSON at line {line}, column {column}")]
    InvalidJson {
        line: usize,
        column: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("JSON Canvas schema violation at {path}: {violation}")]
    Schema { path: String, violation: String },
    #[error("JSON Canvas operation {index} is invalid at {path}: {violation}")]
    Operation {
        index: usize,
        path: String,
        violation: String,
    },
    #[error("JSON Canvas could not be serialised")]
    Serialise {
        #[source]
        source: serde_json::Error,
    },
    #[error("JSON Canvas identifier generation failed")]
    IdentifierGeneration,
}

impl From<(usize, String, &'static str)> for CanvasError {
    fn from(value: (usize, String, &'static str)) -> Self {
        Self::Operation {
            index: value.0,
            path: value.1,
            violation: value.2.to_owned(),
        }
    }
}

impl CanvasError {
    /// Returns the stable machine-readable Canvas error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "format/canvas-schema"
    }

    /// Returns an actionable correction for invalid Canvas data.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        "Correct the reported JSON Canvas schema or operation path, and retry."
    }
}
