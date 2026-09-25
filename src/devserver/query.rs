//! Bounded, observation-scoped queries over published semantics trees.

use super::artifacts::MAX_TREE_BYTES;
use super::events::now_ms;
use super::operations::OperationState;
use super::session::Session;
use gpui_dev_protocol::{ARTIFACT_CHUNK_BYTES, ArtifactKind};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_QUERY_NODES: usize = 200;
pub const MAX_QUERY_BYTES: usize = 128 * 1024;

const DEFAULT_FIELDS: &[&str] = &[
    "node_ref",
    "logical_id",
    "role",
    "name",
    "value",
    "children",
    "source_location",
];
const ALLOWED_FIELDS: &[&str] = &[
    "node_ref",
    "logical_id",
    "role",
    "name",
    "value",
    "enabled",
    "focused",
    "bounds",
    "clip_bounds",
    "children",
    "parent",
    "source_location",
];

#[derive(Clone, Debug, Default)]
pub struct QueryRequest {
    pub observation_id: String,
    pub node_ref: Option<String>,
    pub logical_id: Option<String>,
    pub role: Option<String>,
    pub name: Option<String>,
    pub parent: Option<String>,
    pub fields: Vec<String>,
    pub cursor: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct QueryError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl QueryError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    fn with_details(code: &str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Some(details),
        }
    }
}

pub fn execute(session: &Session, request: QueryRequest) -> Result<Value, QueryError> {
    let operation = session
        .operations
        .find_observation(&request.observation_id, now_ms())
        .map_err(|error| {
            QueryError::with_details(
                &error.code,
                error.message,
                error.details.unwrap_or_else(|| json!({})),
            )
        })?;
    if operation.state != OperationState::Succeeded {
        return Err(QueryError::with_details(
            "observation_unavailable",
            "the observation did not succeed",
            json!({"observation_id": request.observation_id, "state": operation.state}),
        ));
    }
    let result = operation
        .result
        .as_ref()
        .ok_or_else(|| QueryError::new("observation_unavailable", "observation has no result"))?;
    let tree_artifact_id = result["semantics"]["artifact_id"].as_str().ok_or_else(|| {
        QueryError::new(
            "semantics_unavailable",
            "the observation does not contain a semantics tree artifact",
        )
    })?;
    let artifact = session.artifacts.info(tree_artifact_id).map_err(|error| {
        QueryError::with_details(
            error.code.as_str(),
            error.message,
            json!({"artifact_id": tree_artifact_id}),
        )
    })?;
    if artifact.manifest.kind != ArtifactKind::Tree {
        return Err(QueryError::with_details(
            "invalid_artifact_kind",
            "the observation semantics reference is not a tree artifact",
            json!({"artifact_id": tree_artifact_id}),
        ));
    }
    if artifact.manifest.run_id != operation.scope.run_id {
        return Err(QueryError::with_details(
            "stale_observation",
            "the semantics artifact does not belong to the observation run",
            json!({"artifact_id": tree_artifact_id, "run_id": operation.scope.run_id}),
        ));
    }
    let tree = read_tree(session, tree_artifact_id, artifact.manifest.declared_bytes)?;
    query_tree(&tree, &artifact.manifest.sha256, &request, tree_artifact_id)
}

fn read_tree(
    session: &Session,
    artifact_id: &str,
    declared_bytes: u64,
) -> Result<Value, QueryError> {
    if declared_bytes > MAX_TREE_BYTES {
        return Err(QueryError::with_details(
            "artifact_size_exceeded",
            "the semantics tree exceeds the tree artifact limit",
            json!({"artifact_id": artifact_id, "declared_bytes": declared_bytes}),
        ));
    }
    let mut bytes = Vec::with_capacity(declared_bytes as usize);
    let mut offset = 0u64;
    loop {
        let chunk = session
            .artifacts
            .read_chunk(artifact_id, offset, ARTIFACT_CHUNK_BYTES)
            .map_err(|error| {
                QueryError::with_details(
                    error.code.as_str(),
                    error.message,
                    json!({"artifact_id": artifact_id}),
                )
            })?;
        bytes.extend_from_slice(&chunk.data);
        if chunk.eof {
            break;
        }
        if chunk.data.is_empty() || bytes.len() > MAX_TREE_BYTES as usize {
            return Err(QueryError::new(
                "artifact_size_exceeded",
                "the semantics tree exceeded its bounded read size",
            ));
        }
        offset = offset.saturating_add(chunk.data.len() as u64);
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        QueryError::with_details(
            "invalid_tree",
            "the published semantics artifact is not valid JSON",
            json!({"artifact_id": artifact_id, "message": error.to_string()}),
        )
    })
}

fn query_tree(
    tree: &Value,
    artifact_sha256: &str,
    request: &QueryRequest,
    artifact_id: &str,
) -> Result<Value, QueryError> {
    let nodes = tree["nodes"]
        .as_object()
        .ok_or_else(|| QueryError::new("invalid_tree", "the semantics tree has no nodes object"))?;
    if request.limit == 0 || request.limit > MAX_QUERY_NODES {
        return Err(QueryError::with_details(
            "invalid_limit",
            "query limit must be between 1 and 200",
            json!({"max_nodes": MAX_QUERY_NODES}),
        ));
    }
    let fields = normalize_fields(&request.fields)?;
    let filter = json!({
        "node_ref": request.node_ref,
        "logical_id": request.logical_id,
        "role": request.role,
        "name": request.name,
        "parent": request.parent,
        "fields": fields,
        "limit": request.limit,
    });
    let filter_hash = short_hash(&serde_json::to_vec(&filter).unwrap_or_default());
    let artifact_hash = short_hash(artifact_sha256.as_bytes());
    let offset = request
        .cursor
        .as_deref()
        .map(|cursor| decode_cursor(cursor, &artifact_hash, &filter_hash))
        .transpose()?
        .unwrap_or(0);

    let parent_by_child = parent_index(nodes);
    let has_logical_id = nodes.values().any(|node| logical_id(node).is_some());
    if request.logical_id.is_some() && !has_logical_id {
        return Err(QueryError::with_details(
            "unsupported_selector",
            "this semantics tree does not expose stable logical_id values",
            json!({"selector": "logical_id", "unsupported_fields": ["logical_id"]}),
        ));
    }

    let mut matching = nodes
        .iter()
        .filter(|(node_ref, node)| {
            request
                .node_ref
                .as_deref()
                .is_none_or(|expected| expected == node_ref.as_str())
                && request
                    .logical_id
                    .as_deref()
                    .is_none_or(|expected| logical_id(node) == Some(expected))
                && request
                    .role
                    .as_deref()
                    .is_none_or(|expected| role(node) == Some(expected))
                && request
                    .name
                    .as_deref()
                    .is_none_or(|expected| name(node) == Some(expected))
                && request.parent.as_deref().is_none_or(|expected| {
                    parent_by_child.get(node_ref.as_str()).map(String::as_str) == Some(expected)
                })
        })
        .map(|(node_ref, _)| node_ref.clone())
        .collect::<Vec<_>>();
    matching.sort();
    if offset > matching.len() {
        return Err(QueryError::with_details(
            "invalid_cursor",
            "query cursor is past the end of the matching node set",
            json!({"offset": offset, "matches": matching.len()}),
        ));
    }

    let mut page = Vec::new();
    let mut end = offset;
    while end < matching.len() && page.len() < request.limit {
        let node_ref = &matching[end];
        let projected = project_node(
            node_ref,
            nodes.get(node_ref).expect("matching node exists"),
            parent_by_child.get(node_ref).map(String::as_str),
            &fields,
        );
        let mut candidate = page.clone();
        candidate.push(projected);
        if serde_json::to_vec(&candidate).is_ok_and(|bytes| bytes.len() > MAX_QUERY_BYTES) {
            if page.is_empty() {
                return Err(QueryError::with_details(
                    "query_result_too_large",
                    "one projected node exceeds the query response limit",
                    json!({"max_bytes": MAX_QUERY_BYTES, "node_ref": node_ref}),
                ));
            }
            break;
        }
        page = candidate;
        end += 1;
    }
    let next_cursor =
        (end < matching.len()).then(|| encode_cursor(&artifact_hash, &filter_hash, end));
    let unsupported_fields = fields
        .iter()
        .filter(|field| !field_supported(field, nodes, &matching))
        .cloned()
        .collect::<Vec<_>>();
    Ok(json!({
        "observation_id": request.observation_id,
        "artifact_id": artifact_id,
        "artifact_sha256": artifact_sha256,
        "query": filter,
        "nodes": page,
        "returned": end.saturating_sub(offset),
        "omitted": matching.len().saturating_sub(end),
        "next_cursor": next_cursor,
        "unsupported_fields": unsupported_fields,
    }))
}

fn normalize_fields(fields: &[String]) -> Result<Vec<String>, QueryError> {
    let requested: Vec<String> = if fields.is_empty() {
        DEFAULT_FIELDS
            .iter()
            .map(|field| (*field).to_owned())
            .collect()
    } else {
        fields
            .iter()
            .flat_map(|field| field.split(','))
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let mut normalized = BTreeSet::new();
    for field in requested {
        if !ALLOWED_FIELDS.contains(&field.as_str()) {
            return Err(QueryError::with_details(
                "invalid_field",
                format!("unsupported query field `{field}`"),
                json!({"field": field, "allowed": ALLOWED_FIELDS}),
            ));
        }
        normalized.insert(field);
    }
    Ok(normalized.into_iter().collect())
}

fn project_node(node_ref: &str, node: &Value, parent: Option<&str>, fields: &[String]) -> Value {
    let mut projected = serde_json::Map::new();
    for field in fields {
        let value = match field.as_str() {
            "node_ref" => json!(node_ref),
            "logical_id" => logical_id(node).map_or(Value::Null, |value| json!(value)),
            "role" => role(node).map_or(Value::Null, |value| json!(value)),
            "name" => name(node).map_or(Value::Null, |value| json!(value)),
            "value" => aria_value(node, "value"),
            "enabled" => aria_value(node, "enabled"),
            "focused" => aria_value(node, "focused"),
            "bounds" => node.get("bounds").cloned().unwrap_or(Value::Null),
            "clip_bounds" => node.get("clip_bounds").cloned().unwrap_or(Value::Null),
            "children" => node.get("children").cloned().unwrap_or_else(|| json!([])),
            "parent" => parent.map_or(Value::Null, |value| json!(value)),
            "source_location" => node.get("source_location").cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
        projected.insert(field.clone(), value);
    }
    Value::Object(projected)
}

fn field_supported(
    field: &str,
    nodes: &serde_json::Map<String, Value>,
    matching: &[String],
) -> bool {
    matching.iter().any(|node_ref| {
        nodes.get(node_ref).is_some_and(|node| match field {
            "node_ref" | "role" | "name" | "children" => true,
            "logical_id" => logical_id(node).is_some(),
            "value" | "enabled" | "focused" => aria_value(node, field) != Value::Null,
            "bounds" | "clip_bounds" | "source_location" | "parent" => {
                field == "parent" || node.get(field).is_some()
            }
            _ => false,
        })
    })
}

fn aria_value(node: &Value, field: &str) -> Value {
    node["aria"].get(field).cloned().unwrap_or(Value::Null)
}

fn logical_id(node: &Value) -> Option<&str> {
    node.get("logical_id")
        .and_then(Value::as_str)
        .or_else(|| node["aria"]["logical_id"].as_str())
}

fn role(node: &Value) -> Option<&str> {
    node["aria"]["role"].as_str()
}

fn name(node: &Value) -> Option<&str> {
    node["aria"]["name"]
        .as_str()
        .or_else(|| node["aria"]["label"].as_str())
}

fn parent_index(nodes: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    let mut parents = BTreeMap::new();
    for (parent, node) in nodes {
        if let Some(children) = node["children"].as_array() {
            for child in children.iter().filter_map(Value::as_str) {
                parents
                    .entry(child.to_owned())
                    .or_insert_with(|| parent.clone());
            }
        }
    }
    parents
}

fn short_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")[..16].to_owned()
}

fn encode_cursor(artifact_hash: &str, filter_hash: &str, offset: usize) -> String {
    format!("q1-{artifact_hash}-{filter_hash}-{offset}")
}

fn decode_cursor(
    cursor: &str,
    artifact_hash: &str,
    filter_hash: &str,
) -> Result<usize, QueryError> {
    let mut parts = cursor.split('-');
    let valid = parts.next() == Some("q1")
        && parts.next() == Some(artifact_hash)
        && parts.next() == Some(filter_hash);
    let Some(offset) = parts.next().and_then(|value| value.parse::<usize>().ok()) else {
        return Err(QueryError::new(
            "invalid_cursor",
            "query cursor is malformed",
        ));
    };
    if !valid || parts.next().is_some() {
        return Err(QueryError::new(
            "invalid_cursor",
            "query cursor does not match this observation or filter",
        ));
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Value {
        json!({
            "nodes": {
                "a": {"children": ["b", "c"], "aria": {"role": "Window", "label": "Counter"}},
                "b": {"children": [], "aria": {"role": "Button", "label": "Click me", "value": "1"}},
                "c": {"children": [], "aria": {"role": "StaticText", "label": "Count"}}
            }
        })
    }

    #[test]
    fn query_projects_role_name_parent_and_paginates_with_bound_cursor() {
        let request = QueryRequest {
            observation_id: "observation-1".into(),
            role: Some("Button".into()),
            parent: Some("a".into()),
            fields: vec!["node_ref".into(), "name".into(), "value".into()],
            limit: 1,
            ..QueryRequest::default()
        };
        let result = query_tree(&tree(), "sha256:tree", &request, "tree-1").unwrap();
        assert_eq!(result["returned"], 1);
        assert_eq!(result["nodes"][0]["name"], "Click me");
        assert!(result["next_cursor"].is_null());
    }

    #[test]
    fn logical_id_is_not_inferred_from_element_or_node_refs() {
        let request = QueryRequest {
            logical_id: Some("counter.increment".into()),
            limit: 1,
            ..QueryRequest::default()
        };
        let error = query_tree(&tree(), "sha256:tree", &request, "tree-1").unwrap_err();
        assert_eq!(error.code, "unsupported_selector");
    }

    #[test]
    fn cursor_rejects_a_different_filter_binding() {
        let cursor = encode_cursor("artifact-a", "filter-a", 1);
        let error = decode_cursor(&cursor, "artifact-b", "filter-a").unwrap_err();
        assert_eq!(error.code, "invalid_cursor");
    }
}
