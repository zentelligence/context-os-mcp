use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use thiserror::Error;

const BUILT_IN_SUMMARIES: [&str; 15] = [
    "Average",
    "Min",
    "Max",
    "Sum",
    "Range",
    "Median",
    "Stddev",
    "Earliest",
    "Latest",
    "Checked",
    "Unchecked",
    "Empty",
    "Filled",
    "Unique",
    "Count",
];

/// One actionable Bases schema or reference diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseDiagnostic {
    pub code: String,
    pub path: String,
    pub message: String,
}

/// One transactional operation over an Obsidian Bases definition.
#[derive(Clone, Debug, PartialEq)]
pub enum BaseOperation {
    SetFilters {
        filters: Value,
    },
    AddFormula {
        name: String,
        expression: String,
    },
    RemoveFormula {
        name: String,
    },
    SetProperty {
        name: String,
        definition: Map<String, Value>,
    },
    RemoveProperty {
        name: String,
    },
    AddView {
        name: String,
        definition: Map<String, Value>,
    },
    RemoveView {
        name: String,
    },
    UpdateView {
        name: String,
        patch: Map<String, Value>,
    },
    SetSummary {
        name: String,
        expression: String,
    },
    RemoveSummary {
        name: String,
    },
}

/// An ordered Obsidian Bases YAML definition.
#[derive(Clone, Debug, PartialEq)]
pub struct BaseDocument {
    definition: Map<String, Value>,
}

impl TryFrom<&str> for BaseDocument {
    type Error = BaseError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let definition = yaml_serde::from_str::<Map<String, Value>>(source).map_err(|source| {
            let (line, column) = source
                .location()
                .map_or((1, 1), |location| (location.line(), location.column()));
            BaseError::InvalidYaml {
                line,
                column,
                source,
            }
        })?;
        Ok(Self { definition })
    }
}

impl TryFrom<String> for BaseDocument {
    type Error = BaseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl TryFrom<Map<String, Value>> for BaseDocument {
    type Error = BaseError;

    fn try_from(definition: Map<String, Value>) -> Result<Self, Self::Error> {
        let document = Self { definition };
        document.ensure_valid()?;
        Ok(document)
    }
}

impl TryFrom<&BaseDocument> for String {
    type Error = BaseError;

    fn try_from(value: &BaseDocument) -> Result<Self, Self::Error> {
        value.ensure_valid()?;
        yaml_serde::to_string(&value.definition).map_err(|source| BaseError::Serialise { source })
    }
}

impl BaseDocument {
    /// Returns the ordered JSON representation of the Bases YAML definition.
    #[must_use]
    pub const fn definition(&self) -> &Map<String, Value> {
        &self.definition
    }

    /// Reports every schema and resolvable-reference violation.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<BaseDiagnostic> {
        let mut diagnostics = Vec::new();
        for section in self.definition.keys().filter(|section| {
            !matches!(
                section.as_str(),
                "filters" | "formulas" | "properties" | "summaries" | "views"
            )
        }) {
            push_diagnostic(
                &mut diagnostics,
                section,
                "top-level section is not part of the Bases schema",
            );
        }
        validate_filters(self.definition.get("filters"), "filters", &mut diagnostics);
        validate_named_expressions(
            self.definition.get("formulas"),
            "formulas",
            &mut diagnostics,
        );
        validate_properties(
            self.definition.get("properties"),
            "properties",
            &mut diagnostics,
        );
        validate_named_expressions(
            self.definition.get("summaries"),
            "summaries",
            &mut diagnostics,
        );
        validate_views(self.definition.get("views"), &mut diagnostics);
        validate_formula_references(&self.definition, &mut diagnostics);
        validate_summary_references(&self.definition, &mut diagnostics);
        diagnostics
    }

    /// Applies an operation list atomically and validates the complete result.
    ///
    /// # Errors
    ///
    /// Returns the specific operation or final-schema violation without
    /// changing the original document.
    pub fn apply(&mut self, operations: Vec<BaseOperation>) -> Result<(), BaseError> {
        let mut candidate = self.definition.clone();
        for (index, operation) in operations.into_iter().enumerate() {
            apply_operation(&mut candidate, operation, index)?;
        }
        let candidate = Self {
            definition: candidate,
        };
        candidate.ensure_valid()?;
        *self = candidate;
        Ok(())
    }

    fn ensure_valid(&self) -> Result<(), BaseError> {
        match self.diagnostics().into_iter().next() {
            Some(diagnostic) => Err(BaseError::Schema {
                path: diagnostic.path,
                violation: diagnostic.message,
            }),
            None => Ok(()),
        }
    }
}

fn apply_operation(
    definition: &mut Map<String, Value>,
    operation: BaseOperation,
    index: usize,
) -> Result<(), BaseError> {
    match operation {
        BaseOperation::SetFilters { filters } => {
            definition.insert("filters".to_owned(), filters);
        }
        BaseOperation::AddFormula { name, expression } => {
            require_name(&name, index, "formula name")?;
            require_expression(&expression, index, "formula expression")?;
            let formulas = object_section(definition, "formulas", index)?;
            if formulas.contains_key(&name) {
                return Err(BaseError::Operation {
                    index,
                    path: format!("formulas.{name}"),
                    violation: "formula already exists".to_owned(),
                });
            }
            formulas.insert(name, Value::String(expression));
        }
        BaseOperation::RemoveFormula { name } => {
            remove_from_object_section(definition, "formulas", &name, index, "formula")?;
        }
        BaseOperation::SetProperty {
            name,
            definition: property,
        } => {
            require_name(&name, index, "property name")?;
            object_section(definition, "properties", index)?.insert(name, Value::Object(property));
        }
        BaseOperation::RemoveProperty { name } => {
            remove_from_object_section(definition, "properties", &name, index, "property")?;
        }
        BaseOperation::AddView {
            name,
            definition: mut view,
        } => {
            require_name(&name, index, "view name")?;
            let views = array_section(definition, "views", index)?;
            if view_index(views, &name).is_some() {
                return Err(BaseError::Operation {
                    index,
                    path: format!("views.{name}"),
                    violation: "view already exists".to_owned(),
                });
            }
            view.insert("name".to_owned(), Value::String(name));
            views.push(Value::Object(view));
        }
        BaseOperation::RemoveView { name } => {
            let views = array_section(definition, "views", index)?;
            let Some(position) = view_index(views, &name) else {
                return Err(BaseError::Operation {
                    index,
                    path: format!("views.{name}"),
                    violation: "view does not exist".to_owned(),
                });
            };
            views.remove(position);
        }
        BaseOperation::UpdateView { name, patch } => {
            let views = array_section(definition, "views", index)?;
            let Some(position) = view_index(views, &name) else {
                return Err(BaseError::Operation {
                    index,
                    path: format!("views.{name}"),
                    violation: "view does not exist".to_owned(),
                });
            };
            let Some(view) = views.get_mut(position).and_then(Value::as_object_mut) else {
                return Err(BaseError::Operation {
                    index,
                    path: format!("views[{position}]"),
                    violation: "view must be an object".to_owned(),
                });
            };
            merge_patch(view, patch);
            view.insert("name".to_owned(), Value::String(name));
        }
        BaseOperation::SetSummary { name, expression } => {
            require_name(&name, index, "summary name")?;
            require_expression(&expression, index, "summary expression")?;
            object_section(definition, "summaries", index)?.insert(name, Value::String(expression));
        }
        BaseOperation::RemoveSummary { name } => {
            remove_from_object_section(definition, "summaries", &name, index, "summary")?;
        }
    }
    Ok(())
}

fn remove_from_object_section(
    definition: &mut Map<String, Value>,
    section: &'static str,
    name: &str,
    index: usize,
    label: &'static str,
) -> Result<(), BaseError> {
    let entries = object_section(definition, section, index)?;
    if entries.shift_remove(name).is_none() {
        return Err(BaseError::Operation {
            index,
            path: format!("{section}.{name}"),
            violation: format!("{label} does not exist"),
        });
    }
    Ok(())
}

fn object_section<'a>(
    definition: &'a mut Map<String, Value>,
    section: &'static str,
    index: usize,
) -> Result<&'a mut Map<String, Value>, BaseError> {
    let value = definition
        .entry(section.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    value.as_object_mut().ok_or_else(|| BaseError::Operation {
        index,
        path: section.to_owned(),
        violation: "section must be an object".to_owned(),
    })
}

fn array_section<'a>(
    definition: &'a mut Map<String, Value>,
    section: &'static str,
    index: usize,
) -> Result<&'a mut Vec<Value>, BaseError> {
    let value = definition
        .entry(section.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    value.as_array_mut().ok_or_else(|| BaseError::Operation {
        index,
        path: section.to_owned(),
        violation: "section must be an array".to_owned(),
    })
}

fn require_name(name: &str, index: usize, label: &'static str) -> Result<(), BaseError> {
    if name.trim().is_empty() {
        return Err(BaseError::Operation {
            index,
            path: format!("operations[{index}].name"),
            violation: format!("{label} must not be empty"),
        });
    }
    Ok(())
}

fn require_expression(
    expression: &str,
    index: usize,
    label: &'static str,
) -> Result<(), BaseError> {
    if expression.trim().is_empty() {
        return Err(BaseError::Operation {
            index,
            path: format!("operations[{index}].expression"),
            violation: format!("{label} must not be empty"),
        });
    }
    Ok(())
}

fn view_index(views: &[Value], name: &str) -> Option<usize> {
    views.iter().position(|view| {
        view.as_object()
            .and_then(|view| view.get("name"))
            .and_then(Value::as_str)
            == Some(name)
    })
}

fn merge_patch(target: &mut Map<String, Value>, patch: Map<String, Value>) {
    for (key, value) in patch {
        match value {
            Value::Null => {
                target.shift_remove(&key);
            }
            Value::Object(nested_patch) => {
                let nested = target
                    .entry(key)
                    .or_insert_with(|| Value::Object(Map::new()));
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

fn validate_filters(value: Option<&Value>, path: &str, diagnostics: &mut Vec<BaseDiagnostic>) {
    let Some(value) = value else {
        return;
    };
    if value.is_string() {
        if value.as_str().is_some_and(str::is_empty) {
            push_diagnostic(diagnostics, path, "filter expression must not be empty");
        }
        return;
    }
    let Some(filter) = value.as_object() else {
        push_diagnostic(
            diagnostics,
            path,
            "filters must be an expression string or an and/or/not object",
        );
        return;
    };
    if filter.len() != 1 {
        push_diagnostic(
            diagnostics,
            path,
            "filter objects must contain exactly one of and, or, or not",
        );
        return;
    }
    let Some((operator, operands)) = filter.iter().next() else {
        return;
    };
    if !matches!(operator.as_str(), "and" | "or" | "not") {
        push_diagnostic(
            diagnostics,
            path,
            "filter object key must be and, or, or not",
        );
        return;
    }
    let Some(operands) = operands.as_array() else {
        push_diagnostic(
            diagnostics,
            &format!("{path}.{operator}"),
            "filter operands must be an array",
        );
        return;
    };
    for (index, operand) in operands.iter().enumerate() {
        validate_filters(
            Some(operand),
            &format!("{path}.{operator}[{index}]"),
            diagnostics,
        );
    }
}

fn validate_named_expressions(
    value: Option<&Value>,
    path: &str,
    diagnostics: &mut Vec<BaseDiagnostic>,
) {
    let Some(value) = value else {
        return;
    };
    let Some(expressions) = value.as_object() else {
        push_diagnostic(diagnostics, path, "section must be an object");
        return;
    };
    for (name, expression) in expressions {
        if name.trim().is_empty() {
            push_diagnostic(diagnostics, path, "entry name must not be empty");
        }
        if !expression.is_string() || expression.as_str().is_some_and(str::is_empty) {
            push_diagnostic(
                diagnostics,
                &format!("{path}.{name}"),
                "expression must be a non-empty string",
            );
        }
    }
}

fn validate_properties(value: Option<&Value>, path: &str, diagnostics: &mut Vec<BaseDiagnostic>) {
    let Some(value) = value else {
        return;
    };
    let Some(properties) = value.as_object() else {
        push_diagnostic(diagnostics, path, "properties must be an object");
        return;
    };
    for (name, property) in properties {
        let property_path = format!("{path}.{name}");
        let Some(property) = property.as_object() else {
            push_diagnostic(
                diagnostics,
                &property_path,
                "property configuration must be an object",
            );
            continue;
        };
        if property
            .get("displayName")
            .is_some_and(|display| !display.is_string())
        {
            push_diagnostic(
                diagnostics,
                &format!("{property_path}.displayName"),
                "displayName must be a string",
            );
        }
    }
}

fn validate_views(value: Option<&Value>, diagnostics: &mut Vec<BaseDiagnostic>) {
    let Some(value) = value else {
        push_diagnostic(diagnostics, "views", "views must contain at least one view");
        return;
    };
    let Some(views) = value.as_array() else {
        push_diagnostic(diagnostics, "views", "views must be an array");
        return;
    };
    if views.is_empty() {
        push_diagnostic(diagnostics, "views", "views must contain at least one view");
    }
    let mut names = BTreeSet::new();
    for (index, view) in views.iter().enumerate() {
        let path = format!("views[{index}]");
        let Some(view) = view.as_object() else {
            push_diagnostic(diagnostics, &path, "view must be an object");
            continue;
        };
        for field in ["type", "name"] {
            if view
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                push_diagnostic(
                    diagnostics,
                    &format!("{path}.{field}"),
                    &format!("{field} must be a non-empty string"),
                );
            }
        }
        if view
            .get("type")
            .is_some_and(|kind| !matches!(kind.as_str(), Some("table" | "cards" | "list" | "map")))
        {
            push_diagnostic(
                diagnostics,
                &format!("{path}.type"),
                "type must be table, cards, list, or map",
            );
        }
        if let Some(name) = view.get("name").and_then(Value::as_str)
            && !names.insert(name)
        {
            push_diagnostic(
                diagnostics,
                &format!("{path}.name"),
                "view names must be unique",
            );
        }
        if view
            .get("limit")
            .is_some_and(|limit| limit.as_u64().is_none_or(|limit| limit == 0))
        {
            push_diagnostic(
                diagnostics,
                &format!("{path}.limit"),
                "limit must be a positive integer",
            );
        }
        validate_filters(view.get("filters"), &format!("{path}.filters"), diagnostics);
        validate_string_array(view.get("order"), &format!("{path}.order"), diagnostics);
        validate_group_by(view.get("groupBy"), &format!("{path}.groupBy"), diagnostics);
        validate_sort(view.get("sort"), &format!("{path}.sort"), diagnostics);
        if view
            .get("summaries")
            .is_some_and(|summaries| !summaries.is_object())
        {
            push_diagnostic(
                diagnostics,
                &format!("{path}.summaries"),
                "view summaries must be an object",
            );
        }
    }
}

fn validate_string_array(value: Option<&Value>, path: &str, diagnostics: &mut Vec<BaseDiagnostic>) {
    let Some(value) = value else {
        return;
    };
    let Some(values) = value.as_array() else {
        push_diagnostic(diagnostics, path, "value must be an array of strings");
        return;
    };
    for (index, value) in values.iter().enumerate() {
        if !value.is_string() {
            push_diagnostic(
                diagnostics,
                &format!("{path}[{index}]"),
                "value must be a string",
            );
        }
    }
}

fn validate_group_by(value: Option<&Value>, path: &str, diagnostics: &mut Vec<BaseDiagnostic>) {
    let Some(value) = value else {
        return;
    };
    let Some(group) = value.as_object() else {
        push_diagnostic(diagnostics, path, "groupBy must be an object");
        return;
    };
    if !group.get("property").is_some_and(Value::is_string) {
        push_diagnostic(
            diagnostics,
            &format!("{path}.property"),
            "groupBy property must be a string",
        );
    }
    validate_direction(
        group.get("direction"),
        &format!("{path}.direction"),
        diagnostics,
    );
}

fn validate_sort(value: Option<&Value>, path: &str, diagnostics: &mut Vec<BaseDiagnostic>) {
    let Some(value) = value else {
        return;
    };
    let Some(sort) = value.as_array() else {
        push_diagnostic(diagnostics, path, "sort must be an array");
        return;
    };
    for (index, entry) in sort.iter().enumerate() {
        let entry_path = format!("{path}[{index}]");
        let Some(entry) = entry.as_object() else {
            push_diagnostic(diagnostics, &entry_path, "sort entry must be an object");
            continue;
        };
        if !entry.get("property").is_some_and(Value::is_string) {
            push_diagnostic(
                diagnostics,
                &format!("{entry_path}.property"),
                "sort property must be a string",
            );
        }
        validate_direction(
            entry.get("direction"),
            &format!("{entry_path}.direction"),
            diagnostics,
        );
    }
}

fn validate_direction(value: Option<&Value>, path: &str, diagnostics: &mut Vec<BaseDiagnostic>) {
    if value.is_some_and(|direction| !matches!(direction.as_str(), Some("ASC" | "DESC"))) {
        push_diagnostic(diagnostics, path, "direction must be ASC or DESC");
    }
}

fn validate_formula_references(
    definition: &Map<String, Value>,
    diagnostics: &mut Vec<BaseDiagnostic>,
) {
    let formulas = definition
        .get("formulas")
        .and_then(Value::as_object)
        .map(|formulas| formulas.keys().map(String::as_str).collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let mut references = BTreeMap::<String, BTreeSet<String>>::new();
    if let Some(filters) = definition.get("filters") {
        collect_formula_references(filters, "$.filters", &mut references);
    }
    for section in ["formulas", "summaries"] {
        if let Some(expressions) = definition.get(section).and_then(Value::as_object) {
            for (name, expression) in expressions {
                collect_formula_references(
                    expression,
                    &format!("$.{section}.{name}"),
                    &mut references,
                );
            }
        }
    }
    if let Some(views) = definition.get("views").and_then(Value::as_array) {
        for (index, view) in views.iter().enumerate() {
            let Some(view) = view.as_object() else {
                continue;
            };
            for field in ["filters", "order", "groupBy", "sort"] {
                if let Some(value) = view.get(field) {
                    collect_formula_references(
                        value,
                        &format!("$.views[{index}].{field}"),
                        &mut references,
                    );
                }
            }
            if let Some(summaries) = view.get("summaries").and_then(Value::as_object) {
                for property in summaries.keys() {
                    if let Some(name) = property.strip_prefix("formula.") {
                        references
                            .entry(format!("$.views[{index}].summaries.{property}"))
                            .or_default()
                            .insert(name.to_owned());
                    }
                }
            }
        }
    }
    if let Some(properties) = definition.get("properties").and_then(Value::as_object) {
        for name in properties
            .keys()
            .filter_map(|name| name.strip_prefix("formula."))
        {
            references
                .entry(format!("properties.formula.{name}"))
                .or_default()
                .insert(name.to_owned());
        }
    }
    for (path, names) in references {
        for name in names {
            if !formulas.contains(name.as_str()) {
                push_diagnostic(
                    diagnostics,
                    &path,
                    &format!("formula.{name} is not defined"),
                );
            }
        }
    }
}

fn collect_formula_references(
    value: &Value,
    path: &str,
    references: &mut BTreeMap<String, BTreeSet<String>>,
) {
    match value {
        Value::String(expression) => {
            let FormulaReferences(names) = FormulaReferences::from(expression.as_str());
            if !names.is_empty() {
                references.entry(path.to_owned()).or_default().extend(names);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_formula_references(value, &format!("{path}[{index}]"), references);
            }
        }
        Value::Object(values) => {
            for (name, value) in values {
                collect_formula_references(value, &format!("{path}.{name}"), references);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

struct FormulaReferences(Vec<String>);

impl From<&str> for FormulaReferences {
    fn from(value: &str) -> Self {
        let mut names = Vec::new();
        let mut remaining = value;
        while let Some(position) = remaining.find("formula") {
            let after_formula = &remaining[position.saturating_add("formula".len())..];
            if let Some(identifier) = after_formula.strip_prefix('.') {
                let length = identifier
                    .chars()
                    .take_while(|character| {
                        character.is_alphanumeric() || matches!(character, '_' | '-')
                    })
                    .map(char::len_utf8)
                    .sum::<usize>();
                if length > 0 {
                    let name = identifier[..length].to_owned();
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
                remaining = &identifier[length..];
                continue;
            }
            let Some(bracketed) = after_formula.strip_prefix('[') else {
                remaining = after_formula;
                continue;
            };
            let Some(quote @ ('\'' | '"')) = bracketed.chars().next() else {
                remaining = bracketed;
                continue;
            };
            let quoted = &bracketed[quote.len_utf8()..];
            let mut name = String::new();
            let mut escaped = false;
            let mut consumed = None;
            for (offset, character) in quoted.char_indices() {
                if escaped {
                    name.push(character);
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == quote {
                    let after_quote = &quoted[offset.saturating_add(character.len_utf8())..];
                    if after_quote.starts_with(']') {
                        consumed = Some(
                            offset
                                .saturating_add(character.len_utf8())
                                .saturating_add(']'.len_utf8()),
                        );
                    }
                    break;
                } else {
                    name.push(character);
                }
            }
            if let Some(consumed) = consumed {
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
                remaining = &quoted[consumed..];
            } else {
                remaining = quoted;
            }
        }
        Self(names)
    }
}

fn validate_summary_references(
    definition: &Map<String, Value>,
    diagnostics: &mut Vec<BaseDiagnostic>,
) {
    let custom = definition
        .get("summaries")
        .and_then(Value::as_object)
        .map(|summaries| {
            summaries
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let Some(views) = definition.get("views").and_then(Value::as_array) else {
        return;
    };
    for (view_index, view) in views.iter().enumerate() {
        let Some(summaries) = view
            .as_object()
            .and_then(|view| view.get("summaries"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (property, summary) in summaries {
            let Some(summary) = summary.as_str() else {
                push_diagnostic(
                    diagnostics,
                    &format!("views[{view_index}].summaries.{property}"),
                    "summary reference must be a string",
                );
                continue;
            };
            if !BUILT_IN_SUMMARIES.contains(&summary) && !custom.contains(summary) {
                push_diagnostic(
                    diagnostics,
                    &format!("views[{view_index}].summaries.{property}"),
                    &format!("summary {summary} is not defined"),
                );
            }
        }
    }
}

fn push_diagnostic(diagnostics: &mut Vec<BaseDiagnostic>, path: &str, message: &str) {
    diagnostics.push(BaseDiagnostic {
        code: "base/schema".to_owned(),
        path: path.to_owned(),
        message: message.to_owned(),
    });
}

/// Typed Obsidian Bases codec and operation failures.
#[derive(Debug, Error)]
pub enum BaseError {
    #[error("Bases YAML is invalid at line {line}, column {column}")]
    InvalidYaml {
        line: usize,
        column: usize,
        #[source]
        source: yaml_serde::Error,
    },
    #[error("Bases schema violation at {path}: {violation}")]
    Schema { path: String, violation: String },
    #[error("Bases operation {index} is invalid at {path}: {violation}")]
    Operation {
        index: usize,
        path: String,
        violation: String,
    },
    #[error("Bases definition could not be serialised as YAML")]
    Serialise {
        #[source]
        source: yaml_serde::Error,
    },
}

impl BaseError {
    /// Returns the stable machine-readable Bases error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "format/base-schema"
    }

    /// Returns an actionable correction for invalid Bases data.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        "Correct the reported Bases YAML, schema, or operation path, and retry."
    }
}
