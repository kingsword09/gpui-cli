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
pub const MAX_DIFF_ENTRIES: usize = MAX_QUERY_NODES;
pub const MAX_DIFF_BYTES: usize = MAX_QUERY_BYTES;

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

#[derive(Clone, Debug, Default)]
pub struct DiffRequest {
    pub before_observation_id: String,
    pub after_observation_id: String,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct QueryError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

struct LoadedTree {
    observation_id: String,
    run_id: Option<String>,
    artifact_id: String,
    artifact_sha256: String,
    tree: Value,
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
    let loaded = load_tree(session, &request.observation_id)?;
    query_tree(
        &loaded.tree,
        &loaded.artifact_sha256,
        &request,
        &loaded.artifact_id,
    )
}

pub fn execute_diff(session: &Session, request: DiffRequest) -> Result<Value, QueryError> {
    validate_diff_limit(request.limit)?;
    let before = load_tree(session, &request.before_observation_id)?;
    let after = load_tree(session, &request.after_observation_id)?;
    diff_trees(&before, &after, request.limit)
}

fn load_tree(session: &Session, observation_id: &str) -> Result<LoadedTree, QueryError> {
    let operation = session
        .operations
        .find_observation(observation_id, now_ms())
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
            json!({"observation_id": observation_id, "state": operation.state}),
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
    if tree["nodes"].as_object().is_none() {
        return Err(QueryError::new(
            "invalid_tree",
            "the semantics tree has no nodes object",
        ));
    }
    Ok(LoadedTree {
        observation_id: observation_id.to_owned(),
        run_id: operation.scope.run_id,
        artifact_id: tree_artifact_id.to_owned(),
        artifact_sha256: artifact.manifest.sha256,
        tree,
    })
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

#[derive(Debug)]
struct TreeIndex {
    stable_by_id: BTreeMap<String, String>,
    stable_by_ref: BTreeMap<String, String>,
    unstable_roots: Vec<UnstableSubtree>,
}

#[derive(Debug)]
struct UnstableSubtree {
    node_ref: String,
    logical_id: Option<String>,
    reason: &'static str,
    node_count: usize,
}

fn diff_trees(before: &LoadedTree, after: &LoadedTree, limit: usize) -> Result<Value, QueryError> {
    let before_nodes = before.tree["nodes"]
        .as_object()
        .expect("loaded tree nodes were validated");
    let after_nodes = after.tree["nodes"]
        .as_object()
        .expect("loaded tree nodes were validated");
    let before_index = build_tree_index(&before.tree, before_nodes);
    let after_index = build_tree_index(&after.tree, after_nodes);

    let mut logical_ids = BTreeSet::new();
    logical_ids.extend(before_index.stable_by_id.keys().cloned());
    logical_ids.extend(after_index.stable_by_id.keys().cloned());

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged = 0usize;

    for logical_id in logical_ids {
        match (
            before_index.stable_by_id.get(&logical_id),
            after_index.stable_by_id.get(&logical_id),
        ) {
            (None, Some(after_ref)) => added.push(node_payload(
                &logical_id,
                after_ref,
                after_nodes
                    .get(after_ref)
                    .expect("stable node reference exists"),
                &after_index.stable_by_ref,
            )),
            (Some(before_ref), None) => removed.push(node_payload(
                &logical_id,
                before_ref,
                before_nodes
                    .get(before_ref)
                    .expect("stable node reference exists"),
                &before_index.stable_by_ref,
            )),
            (Some(before_ref), Some(after_ref)) => {
                let before_node = before_nodes
                    .get(before_ref)
                    .expect("stable node reference exists");
                let after_node = after_nodes
                    .get(after_ref)
                    .expect("stable node reference exists");
                let before_fields = semantic_fields(before_node, &before_index.stable_by_ref);
                let after_fields = semantic_fields(after_node, &after_index.stable_by_ref);
                let changes = field_changes(&before_fields, &after_fields);
                if changes.is_empty() {
                    unchanged += 1;
                } else {
                    changed.push(json!({
                        "logical_id": logical_id,
                        "before_node_ref": before_ref,
                        "after_node_ref": after_ref,
                        "changes": changes,
                    }));
                }
            }
            (None, None) => unreachable!("logical id came from one of the indexes"),
        }
    }

    let mut subtree_replaced = Vec::new();
    subtree_replaced.extend(subtree_payloads("before", &before_index));
    subtree_replaced.extend(subtree_payloads("after", &after_index));

    let unstable_count = subtree_replaced.len();
    let stable_count = before_index.stable_by_id.len() + after_index.stable_by_id.len();
    let comparison = if unstable_count == 0 {
        "stable_ids"
    } else if stable_count == 0 {
        "subtree_replaced"
    } else {
        "stable_ids_with_subtree_replacements"
    };
    let same_run = before.run_id.is_some() && before.run_id == after.run_id;
    let summary = json!({
        "added": added.len(),
        "removed": removed.len(),
        "changed": changed.len(),
        "unchanged": unchanged,
        "subtree_replaced": unstable_count,
    });

    let mut response = json!({
        "before": {
            "observation_id": before.observation_id,
            "run_id": before.run_id,
            "artifact_id": before.artifact_id,
            "artifact_sha256": before.artifact_sha256,
        },
        "after": {
            "observation_id": after.observation_id,
            "run_id": after.run_id,
            "artifact_id": after.artifact_id,
            "artifact_sha256": after.artifact_sha256,
        },
        "same_run": same_run,
        "comparison": comparison,
        "summary": summary,
        "added": [],
        "removed": [],
        "changed": [],
        "subtree_replaced": [],
        "returned": 0,
        "omitted": {},
    });
    let mut omitted = BTreeMap::<&str, usize>::new();
    let categories = [
        ("added", added),
        ("removed", removed),
        ("changed", changed),
        ("subtree_replaced", subtree_replaced),
    ];
    let mut returned = 0usize;
    for (category, entries) in categories {
        for entry in entries {
            if returned >= limit {
                *omitted.entry(category).or_default() += 1;
                continue;
            }
            response[category]
                .as_array_mut()
                .expect("diff category is an array")
                .push(entry);
            if serde_json::to_vec(&response).is_ok_and(|bytes| bytes.len() <= MAX_DIFF_BYTES) {
                returned += 1;
            } else {
                response[category]
                    .as_array_mut()
                    .expect("diff category is an array")
                    .pop();
                *omitted.entry(category).or_default() += 1;
            }
        }
    }
    response["returned"] = json!(returned);
    response["omitted"] = serde_json::to_value(omitted).expect("diff omission counts serialize");
    Ok(response)
}

fn validate_diff_limit(limit: usize) -> Result<(), QueryError> {
    if limit == 0 || limit > MAX_DIFF_ENTRIES {
        return Err(QueryError::with_details(
            "invalid_limit",
            format!("diff limit must be between 1 and {MAX_DIFF_ENTRIES}"),
            json!({"max_entries": MAX_DIFF_ENTRIES}),
        ));
    }
    Ok(())
}

fn build_tree_index(tree: &Value, nodes: &serde_json::Map<String, Value>) -> TreeIndex {
    let parents = parent_index(nodes);
    let mut id_refs = BTreeMap::<String, Vec<String>>::new();
    let mut intrinsic_unstable = BTreeMap::<String, (Option<String>, &'static str)>::new();

    for (node_ref, node) in nodes {
        if let Some(logical_id) = stable_logical_id(node) {
            id_refs
                .entry(logical_id)
                .or_default()
                .push(node_ref.clone());
        } else {
            intrinsic_unstable.insert(node_ref.clone(), (None, "missing_logical_id"));
        }
    }
    for (logical_id, refs) in &id_refs {
        if refs.len() > 1 {
            for node_ref in refs {
                intrinsic_unstable.insert(
                    node_ref.clone(),
                    (Some(logical_id.clone()), "duplicate_logical_id"),
                );
            }
        }
    }

    let mut opaque_refs = BTreeSet::new();
    for node_ref in intrinsic_unstable.keys() {
        let mut pending = vec![node_ref.clone()];
        while let Some(current) = pending.pop() {
            if !opaque_refs.insert(current.clone()) {
                continue;
            }
            if let Some(children) = nodes
                .get(&current)
                .and_then(|node| node["children"].as_array())
            {
                pending.extend(children.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
    }

    let stable_by_id = id_refs
        .into_iter()
        .filter_map(|(logical_id, refs)| {
            (refs.len() == 1 && !opaque_refs.contains(&refs[0]))
                .then(|| (logical_id, refs.into_iter().next().expect("one ref")))
        })
        .collect::<BTreeMap<_, _>>();
    let stable_by_ref = stable_by_id
        .iter()
        .map(|(logical_id, node_ref)| (node_ref.clone(), logical_id.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut unstable_roots = intrinsic_unstable
        .iter()
        .filter(|(node_ref, _)| {
            parents
                .get(node_ref.as_str())
                .is_none_or(|parent| !opaque_refs.contains(parent))
        })
        .map(|(node_ref, (logical_id, reason))| UnstableSubtree {
            node_ref: node_ref.clone(),
            logical_id: logical_id.clone(),
            reason,
            node_count: opaque_subtree_size(node_ref, nodes, &opaque_refs),
        })
        .collect::<Vec<_>>();
    if unstable_roots.is_empty() && stable_by_id.is_empty() && !nodes.is_empty() {
        unstable_roots.push(UnstableSubtree {
            node_ref: tree["root"].as_str().unwrap_or("<tree>").to_owned(),
            logical_id: None,
            reason: "no_stable_logical_ids",
            node_count: nodes.len(),
        });
    }
    unstable_roots.sort_by(|left, right| left.node_ref.cmp(&right.node_ref));

    TreeIndex {
        stable_by_id,
        stable_by_ref,
        unstable_roots,
    }
}

fn opaque_subtree_size(
    root: &str,
    nodes: &serde_json::Map<String, Value>,
    opaque_refs: &BTreeSet<String>,
) -> usize {
    let mut count = 0;
    let mut pending = vec![root.to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(node_ref) = pending.pop() {
        if !visited.insert(node_ref.clone()) || !opaque_refs.contains(&node_ref) {
            continue;
        }
        count += 1;
        if let Some(children) = nodes
            .get(&node_ref)
            .and_then(|node| node["children"].as_array())
        {
            pending.extend(children.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    count
}

fn subtree_payloads(side: &str, index: &TreeIndex) -> Vec<Value> {
    index
        .unstable_roots
        .iter()
        .map(|subtree| {
            json!({
                "side": side,
                "node_ref": subtree.node_ref,
                "logical_id": subtree.logical_id,
                "reason": subtree.reason,
                "node_count": subtree.node_count,
            })
        })
        .collect()
}

fn node_payload(
    logical_id: &str,
    node_ref: &str,
    node: &Value,
    stable_by_ref: &BTreeMap<String, String>,
) -> Value {
    json!({
        "logical_id": logical_id,
        "node_ref": node_ref,
        "fields": semantic_fields(node, stable_by_ref),
    })
}

fn semantic_fields(
    node: &Value,
    stable_by_ref: &BTreeMap<String, String>,
) -> BTreeMap<String, Value> {
    let children = match node.get("children") {
        Some(Value::Array(children)) => Value::Array(
            children
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|node_ref| stable_by_ref.get(node_ref).map(|id| json!(id)))
                .collect(),
        ),
        Some(_) => Value::Null,
        None => json!([]),
    };
    BTreeMap::from([
        (
            "role".into(),
            role(node).map_or(Value::Null, |value| json!(value)),
        ),
        (
            "name".into(),
            name(node).map_or(Value::Null, |value| json!(value)),
        ),
        ("value".into(), aria_value(node, "value")),
        ("enabled".into(), aria_value(node, "enabled")),
        ("focused".into(), aria_value(node, "focused")),
        (
            "bounds".into(),
            node.get("bounds").cloned().unwrap_or(Value::Null),
        ),
        (
            "clip_bounds".into(),
            node.get("clip_bounds").cloned().unwrap_or(Value::Null),
        ),
        ("children".into(), children),
        (
            "source_location".into(),
            node.get("source_location").cloned().unwrap_or(Value::Null),
        ),
    ])
}

fn field_changes(before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) -> Vec<Value> {
    let mut fields = BTreeSet::new();
    fields.extend(before.keys().cloned());
    fields.extend(after.keys().cloned());
    fields
        .into_iter()
        .filter_map(|field| {
            let before_value = before.get(&field).cloned().unwrap_or(Value::Null);
            let after_value = after.get(&field).cloned().unwrap_or(Value::Null);
            (before_value != after_value)
                .then(|| json!({"field": field, "before": before_value, "after": after_value}))
        })
        .collect()
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

fn stable_logical_id(node: &Value) -> Option<String> {
    logical_id(node)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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

    fn loaded(observation_id: &str, run_id: &str, artifact_id: &str, tree: Value) -> LoadedTree {
        LoadedTree {
            observation_id: observation_id.into(),
            run_id: Some(run_id.into()),
            artifact_id: artifact_id.into(),
            artifact_sha256: format!("sha256:{artifact_id}"),
            tree,
        }
    }

    #[test]
    fn diff_matches_unique_logical_ids_and_ignores_temporary_node_refs() {
        let before = loaded(
            "observation-before",
            "run-1",
            "tree-before",
            json!({
                "root": "a",
                "nodes": {
                    "a": {"logical_id": "app", "children": ["b"], "aria": {"role": "Window", "label": "Counter"}},
                    "b": {"logical_id": "counter.value", "children": [], "aria": {"role": "StaticText", "label": "Count", "value": "0"}},
                    "c": {"logical_id": "old", "children": [], "aria": {"role": "StaticText", "label": "Old"}}
                }
            }),
        );
        let after = loaded(
            "observation-after",
            "run-1",
            "tree-after",
            json!({
                "root": "x",
                "nodes": {
                    "x": {"logical_id": "app", "children": ["y"], "aria": {"role": "Window", "label": "Counter"}},
                    "y": {"logical_id": "counter.value", "children": [], "aria": {"role": "StaticText", "label": "Count", "value": "1"}},
                    "z": {"logical_id": "new", "children": [], "aria": {"role": "Button", "label": "New"}}
                }
            }),
        );

        let result = diff_trees(&before, &after, MAX_DIFF_ENTRIES).unwrap();
        assert_eq!(result["comparison"], "stable_ids");
        assert_eq!(result["same_run"], true);
        assert_eq!(result["summary"]["added"], 1);
        assert_eq!(result["summary"]["removed"], 1);
        assert_eq!(result["summary"]["changed"], 1);
        assert_eq!(result["added"][0]["logical_id"], "new");
        assert_eq!(result["removed"][0]["logical_id"], "old");
        assert_eq!(result["changed"][0]["logical_id"], "counter.value");
        assert_eq!(result["changed"][0]["changes"][0]["field"], "value");
        assert_eq!(result["changed"][0]["changes"][0]["before"], "0");
        assert_eq!(result["changed"][0]["changes"][0]["after"], "1");
    }

    #[test]
    fn diff_reports_unstable_subtrees_without_matching_by_node_ref() {
        let before = loaded(
            "observation-before",
            "run-1",
            "tree-before",
            json!({
                "root": "a",
                "nodes": {
                    "a": {"children": ["b"], "aria": {"role": "Window", "label": "Counter"}},
                    "b": {"logical_id": "counter.value", "children": [], "aria": {"role": "StaticText", "value": "0"}}
                }
            }),
        );
        let after = loaded(
            "observation-after",
            "run-2",
            "tree-after",
            json!({
                "root": "x",
                "nodes": {
                    "x": {"children": ["y"], "aria": {"role": "Window", "label": "Counter"}},
                    "y": {"logical_id": "counter.value", "children": [], "aria": {"role": "StaticText", "value": "1"}}
                }
            }),
        );

        let result = diff_trees(&before, &after, MAX_DIFF_ENTRIES).unwrap();
        assert_eq!(result["comparison"], "subtree_replaced");
        assert_eq!(result["same_run"], false);
        assert_eq!(result["summary"]["added"], 0);
        assert_eq!(result["summary"]["removed"], 0);
        assert_eq!(result["summary"]["changed"], 0);
        assert_eq!(result["summary"]["subtree_replaced"], 2);
        assert_eq!(
            result["subtree_replaced"][0]["reason"],
            "missing_logical_id"
        );
        assert_eq!(result["subtree_replaced"][0]["node_count"], 2);
        assert_eq!(result["subtree_replaced"][1]["node_ref"], "x");
    }

    #[test]
    fn diff_limit_reports_omitted_changes_without_breaking_the_summary() {
        let before = loaded(
            "observation-before",
            "run-1",
            "tree-before",
            json!({
                "nodes": {
                    "a": {"logical_id": "one", "children": [], "aria": {"role": "StaticText", "value": "a"}},
                    "b": {"logical_id": "two", "children": [], "aria": {"role": "StaticText", "value": "b"}}
                }
            }),
        );
        let after = loaded(
            "observation-after",
            "run-1",
            "tree-after",
            json!({
                "nodes": {
                    "x": {"logical_id": "one", "children": [], "aria": {"role": "StaticText", "value": "1"}},
                    "y": {"logical_id": "two", "children": [], "aria": {"role": "StaticText", "value": "2"}}
                }
            }),
        );

        let result = diff_trees(&before, &after, 1).unwrap();
        assert_eq!(result["summary"]["changed"], 2);
        assert_eq!(result["returned"], 1);
        assert_eq!(result["omitted"]["changed"], 1);
    }
}
