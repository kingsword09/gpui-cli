use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use regex::{Regex, RegexBuilder};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

#[derive(Debug, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

#[derive(Debug, PartialEq)]
enum Number {
    Signed(i128),
    Unsigned(u128),
    Float(f64),
}

impl Value {
    fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    fn as_i128(&self) -> Option<i128> {
        match self {
            Self::Number(Number::Signed(value)) => Some(*value),
            Self::Number(Number::Unsigned(value)) => i128::try_from(*value).ok(),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(Number::Signed(value)) => Some(*value as f64),
            Self::Number(Number::Unsigned(value)) => Some(*value as f64),
            Self::Number(Number::Float(value)) => Some(*value),
            _ => None,
        }
    }

    fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Self::Object(values) => Some(values),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
}

struct UniqueJsonSeed;

impl<'de> DeserializeSeed<'de> for UniqueJsonSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

struct UniqueJsonVisitor;

impl<'de> Visitor<'de> for UniqueJsonVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(Number::Signed(value as i128)))
    }

    fn visit_i128<E>(self, value: i128) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(Number::Signed(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(Number::Unsigned(value as u128)))
    }

    fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(Number::Unsigned(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(Number::Float(value)))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(UniqueJsonSeed)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate JSON key: {key}")));
            }
            let value = map.next_value_seed(UniqueJsonSeed)?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

fn parse_json(text: &str) -> Result<Value, String> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = UniqueJsonSeed
        .deserialize(&mut deserializer)
        .map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())?;
    Ok(value)
}

fn from_toml(value: toml::Value) -> Value {
    match value {
        toml::Value::String(value) => Value::String(value),
        toml::Value::Integer(value) => Value::Number(Number::Signed(value as i128)),
        toml::Value::Float(value) => Value::Number(Number::Float(value)),
        toml::Value::Boolean(value) => Value::Bool(value),
        toml::Value::Datetime(value) => Value::String(value.to_string()),
        toml::Value::Array(values) => Value::Array(values.into_iter().map(from_toml).collect()),
        toml::Value::Table(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, from_toml(value)))
                .collect(),
        ),
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.as_object()?.get(name)
}

fn field_array<'a>(value: &'a Value, name: &str) -> Option<&'a [Value]> {
    field(value, name)?.as_array()
}

fn field_bool(value: &Value, name: &str) -> Option<bool> {
    field(value, name)?.as_bool()
}

fn field_i128(value: &Value, name: &str) -> Option<i128> {
    field(value, name)?.as_i128()
}

fn field_object<'a>(value: &'a Value, name: &str) -> Option<&'a BTreeMap<String, Value>> {
    field(value, name)?.as_object()
}

fn field_str<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    field(value, name)?.as_str()
}

fn path<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .try_fold(value, |value, name| field(value, name))
}

fn path_str<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    path(value, names)?.as_str()
}

fn path_f64(value: &Value, names: &[&str]) -> Option<f64> {
    path(value, names)?.as_f64()
}

fn collect_files(
    directory: &Path,
    extension: &str,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_files(&path, extension, files)?;
        } else if file_type.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some(extension)
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(())
}

fn workspace_root() -> PathBuf {
    option_env!("CARGO_WORKSPACE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."))
}

fn is_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.chars().enumerate().all(|(index, character)| {
            character.is_ascii_alphabetic()
                || (index > 0
                    && (character.is_ascii_digit() || matches!(character, '+' | '-' | '.')))
        })
}

fn is_external_link(value: &str) -> bool {
    value.starts_with("//") || is_scheme(value)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex_value(bytes[index + 1]);
            let low = hex_value(bytes[index + 2]);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(component) => normalized.push(component),
        }
    }
    normalized
}

fn safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.starts_with('\\')
        && !value.contains('\\')
        && !value.contains(':')
        && !value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
}

fn lowercase_key(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

fn action_type(value: Option<&str>) -> bool {
    matches!(value, Some("click" | "type_text" | "key" | "scroll"))
}

fn has_timezone(value: &str) -> bool {
    value.ends_with('Z')
        || Regex::new(r"[+-]\d{2}:\d{2}$")
            .expect("timezone regex is valid")
            .is_match(value)
}

fn topological_order(graph: &BTreeMap<String, Vec<String>>) -> Result<Vec<String>, String> {
    let known: BTreeSet<&str> = graph.keys().map(String::as_str).collect();
    let missing: BTreeSet<&str> = graph
        .values()
        .flat_map(|dependencies| dependencies.iter().map(String::as_str))
        .filter(|dependency| !known.contains(dependency))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "unknown dependencies: {}",
            missing.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    fn visit(
        node: &str,
        graph: &BTreeMap<String, Vec<String>>,
        done: &mut HashSet<String>,
        stack: &mut Vec<String>,
        order: &mut Vec<String>,
    ) -> Result<(), String> {
        if done.contains(node) {
            return Ok(());
        }
        if let Some(position) = stack.iter().position(|item| item == node) {
            let mut cycle = stack[position..].to_vec();
            cycle.push(node.to_owned());
            return Err(format!("dependency cycle: {}", cycle.join(" -> ")));
        }
        stack.push(node.to_owned());
        for dependency in &graph[node] {
            visit(dependency, graph, done, stack, order)?;
        }
        stack.pop();
        done.insert(node.to_owned());
        order.push(node.to_owned());
        Ok(())
    }

    let mut done = HashSet::new();
    let mut stack = Vec::new();
    let mut order = Vec::new();
    for node in graph.keys() {
        visit(node, graph, &mut done, &mut stack, &mut order)?;
    }
    Ok(order)
}

struct Validator {
    root: PathBuf,
    docs: PathBuf,
    examples: PathBuf,
    errors: Vec<String>,
    link_count: usize,
    link_re: Regex,
    task_re: Regex,
    case_re: Regex,
    sha256_re: Regex,
    task_heading_re: Regex,
    case_heading_re: Regex,
    anchor_re: Regex,
    inline_link_re: Regex,
    heading_re: Regex,
}

impl Validator {
    fn new(root: PathBuf) -> Self {
        Self {
            docs: root.join("docs"),
            examples: root.join("docs/examples"),
            root,
            errors: Vec::new(),
            link_count: 0,
            link_re: Regex::new(r#"!?\[[^\]\n]*\]\((?:<([^>]+)>|([^\s)]+))(?:\s+"[^"]*")?\)"#)
                .expect("link regex is valid"),
            task_re: Regex::new(r"\b[AFGMOPQST]\d{2}\b").expect("task regex is valid"),
            case_re: Regex::new(r"\b[ACMOPRST]-\d{2}\b").expect("case regex is valid"),
            sha256_re: Regex::new(r"[0-9a-f]{64}").expect("hash regex is valid"),
            task_heading_re: RegexBuilder::new(r"^### ([A-Z]\d{2}) · ")
                .multi_line(true)
                .build()
                .expect("task heading regex is valid"),
            case_heading_re: RegexBuilder::new(r"^### ([A-Z]-\d{2}) · ")
                .multi_line(true)
                .build()
                .expect("case heading regex is valid"),
            anchor_re: Regex::new(r#"<a\s+(?:id|name)="([^"]+)""#).expect("anchor regex is valid"),
            inline_link_re: Regex::new(r"\[([^]]+)\]\([^)]*\)")
                .expect("inline link regex is valid"),
            heading_re: RegexBuilder::new(r"^#{1,6}\s+(.+?)(?:\s+#+)?$")
                .multi_line(true)
                .build()
                .expect("heading regex is valid"),
        }
    }

    fn require(&mut self, condition: bool, message: impl Into<String>) {
        if !condition {
            self.errors.push(message.into());
        }
    }

    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    fn prose(&self, text: &str) -> String {
        let mut lines = Vec::new();
        let mut fence: Option<(char, usize)> = None;
        for line in text.lines() {
            let marker = fence_marker(line);
            if let Some((character, length)) = marker {
                match fence {
                    None => fence = Some((character, length)),
                    Some((open_character, open_length))
                        if character == open_character && length >= open_length =>
                    {
                        fence = None
                    }
                    Some(_) => {}
                }
                lines.push(String::new());
            } else if fence.is_none() {
                lines.push(line.to_owned());
            } else {
                lines.push(String::new());
            }
        }
        lines.join("\n")
    }

    fn anchors(&self, text: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        for capture in self.anchor_re.captures_iter(text) {
            result.insert(capture[1].to_owned());
        }

        let mut seen = HashMap::<String, usize>::new();
        let prose = self.prose(text);
        for capture in self.heading_re.captures_iter(&prose) {
            let heading = self.inline_link_re.replace_all(&capture[1], "$1");
            let slug: String = heading
                .chars()
                .filter(|character| {
                    character.is_alphanumeric() || matches!(character, '_' | '-' | ' ')
                })
                .flat_map(|character| {
                    if character == ' ' {
                        "-".chars().collect::<Vec<_>>()
                    } else {
                        character.to_lowercase().collect::<Vec<_>>()
                    }
                })
                .collect();
            let count = seen.entry(slug.clone()).or_insert(0);
            let anchor = if *count == 0 {
                slug
            } else {
                format!("{slug}-{}", *count)
            };
            *count += 1;
            result.insert(anchor);
        }
        result
    }

    fn links(&mut self, path: &Path, text: &str) {
        let prose = self.prose(text);
        let links: Vec<(String, String)> = self
            .link_re
            .captures_iter(&prose)
            .map(|capture| {
                let target = capture
                    .get(1)
                    .or_else(|| capture.get(2))
                    .expect("link target capture exists")
                    .as_str()
                    .to_owned();
                (target, capture[0].to_owned())
            })
            .collect();
        for (target, matched_link) in links {
            let target = target.as_str();
            if is_external_link(target) {
                continue;
            }

            let (target_without_fragment, fragment) = target
                .split_once('#')
                .map_or((target, None), |(path, fragment)| (path, Some(fragment)));
            let target_path = target_without_fragment
                .split_once('?')
                .map_or(target_without_fragment, |(path, _)| path);
            let target_path = percent_decode(target_path);
            let destination = normalize_path(&if Path::new(&target_path).is_absolute() {
                PathBuf::from(target_path)
            } else {
                path.parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(target_path)
            });
            let label = format!("{}: {}", self.display_path(path), matched_link);
            self.require(
                destination.starts_with(&self.root),
                format!("link escapes repository: {label}"),
            );
            self.require(
                destination.exists(),
                format!("missing link target: {label}"),
            );
            self.link_count += 1;

            if let Some(fragment) = fragment
                && destination.is_file()
                && destination.extension().and_then(|value| value.to_str()) == Some("md")
            {
                let anchors = fs::read_to_string(&destination)
                    .map(|content| self.anchors(&content).contains(&percent_decode(fragment)))
                    .unwrap_or(false);
                self.require(anchors, format!("missing heading anchor: {label}"));
            }
        }
    }

    fn indexes(&mut self, documents: &[(PathBuf, String)]) -> (usize, usize) {
        let backlog_path = self.root.join("docs/roadmap/implementation-backlog.md");
        let acceptance_path = self.root.join("docs/roadmap/acceptance-matrix.md");
        let Some(backlog) = documents
            .iter()
            .find(|(path, _)| path == &backlog_path)
            .map(|(_, text)| text.as_str())
        else {
            self.errors
                .push(format!("missing {}", self.display_path(&backlog_path)));
            return (0, 0);
        };
        let Some(acceptance) = documents
            .iter()
            .find(|(path, _)| path == &acceptance_path)
            .map(|(_, text)| text.as_str())
        else {
            self.errors
                .push(format!("missing {}", self.display_path(&acceptance_path)));
            return (0, 0);
        };

        let backlog_prose = self.prose(backlog);
        let acceptance_prose = self.prose(acceptance);
        let headings: BTreeSet<String> = self
            .task_heading_re
            .captures_iter(&backlog_prose)
            .map(|capture| capture[1].to_owned())
            .collect();
        let heading_count = self.task_heading_re.captures_iter(&backlog_prose).count();
        self.require(heading_count == headings.len(), "duplicate task headings");

        let cases: BTreeSet<String> = self
            .case_heading_re
            .captures_iter(&acceptance_prose)
            .map(|capture| capture[1].to_owned())
            .collect();
        let case_heading_count = self
            .case_heading_re
            .captures_iter(&acceptance_prose)
            .count();
        self.require(
            case_heading_count == cases.len(),
            "duplicate acceptance headings",
        );

        let mut graph = BTreeMap::new();
        let mut covered = BTreeSet::new();
        for line in backlog.lines() {
            let cells: Vec<_> = line
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            if cells.first().is_none_or(|cell| {
                !self.task_re.is_match(cell)
                    || !self
                        .task_re
                        .find(cell)
                        .is_some_and(|match_| match_.as_str() == *cell)
            }) {
                continue;
            }
            let task_id = cells[0].to_owned();
            self.require(
                cells.len() == 7,
                format!("{task_id}: expected seven task-table columns"),
            );
            if cells.len() != 7 {
                continue;
            }
            self.require(
                !graph.contains_key(&task_id),
                format!("duplicate task row: {task_id}"),
            );
            self.require(
                matches!(cells[1], "G0" | "G1" | "G2" | "G3" | "G4"),
                format!("{task_id}: invalid gate"),
            );
            self.require(
                matches!(cells[2], "core" | "optional"),
                format!("{task_id}: invalid priority"),
            );
            self.require(
                matches!(cells[3], "S" | "M" | "L"),
                format!("{task_id}: invalid size"),
            );
            let dependencies = if cells[4] == "—" {
                Vec::new()
            } else {
                cells[4]
                    .split(',')
                    .map(|item| item.trim().to_owned())
                    .collect()
            };
            self.require(
                matches!(
                    cells[6],
                    "planned"
                        | "in_progress"
                        | "in_review"
                        | "done"
                        | "blocked_by_experiment"
                        | "deferred"
                ),
                format!("{task_id}: invalid status"),
            );
            let case_ids: Vec<String> = cells[5]
                .split(',')
                .map(|item| item.trim().to_owned())
                .collect();
            self.require(
                !case_ids.is_empty() && case_ids.iter().all(|case| cases.contains(case)),
                format!("{task_id}: missing/unknown acceptance IDs"),
            );
            covered.extend(case_ids);
            graph.insert(task_id, dependencies);
        }

        self.require(
            graph.keys().collect::<BTreeSet<_>>() == headings.iter().collect::<BTreeSet<_>>(),
            "task table and task sections differ",
        );
        let uncovered: Vec<_> = cases.difference(&covered).cloned().collect();
        self.require(
            uncovered.is_empty(),
            format!("acceptance cases without a task: {uncovered:?}"),
        );
        if let Err(error) = topological_order(&graph) {
            self.errors.push(error);
        }

        for (path, text) in documents {
            let task_refs: BTreeSet<String> = self
                .task_re
                .find_iter(text)
                .map(|match_| match_.as_str().to_owned())
                .filter(|task| !matches!(task.as_str(), "P50" | "P95" | "P99"))
                .collect();
            let unknown_tasks: Vec<_> = task_refs
                .difference(&graph.keys().cloned().collect())
                .cloned()
                .collect();
            self.require(
                unknown_tasks.is_empty(),
                format!(
                    "{}: unknown tasks {unknown_tasks:?}",
                    self.display_path(path)
                ),
            );
            let case_refs: BTreeSet<String> = self
                .case_re
                .find_iter(text)
                .map(|match_| match_.as_str().to_owned())
                .collect();
            let unknown_cases: Vec<_> = case_refs.difference(&cases).cloned().collect();
            self.require(
                unknown_cases.is_empty(),
                format!(
                    "{}: unknown cases {unknown_cases:?}",
                    self.display_path(path)
                ),
            );
        }
        (graph.len(), cases.len())
    }

    fn hashes(&mut self, value: &Value, location: &str) {
        match value {
            Value::Object(values) => {
                for (key, child) in values {
                    let child_location = format!("{location}.{key}");
                    if key == "sha256" || key.ends_with("_sha256") || key.ends_with("_hash") {
                        self.require(
                            child.as_str().is_some_and(|value| {
                                self.sha256_re.is_match(value) && value.len() == 64
                            }),
                            format!("invalid illustrative SHA-256: {child_location}"),
                        );
                    }
                    if key == "content_id" {
                        self.require(
                            child.as_str().is_some_and(|value| {
                                value.len() == 71
                                    && value.starts_with("sha256:")
                                    && self.sha256_re.is_match(&value[7..])
                            }),
                            format!("invalid content ID: {child_location}"),
                        );
                    }
                    self.hashes(child, &child_location);
                }
            }
            Value::Array(values) => {
                for (index, child) in values.iter().enumerate() {
                    self.hashes(child, &format!("{location}[{index}]"));
                }
            }
            _ => {}
        }
    }

    fn examples(&mut self, parsed: &BTreeMap<String, Value>) {
        let Some(scenarios_value) = parsed.get("scenarios.toml") else {
            self.errors.push("missing scenarios.toml".to_owned());
            return;
        };
        self.require(
            field_i128(scenarios_value, "schema_version") == Some(1),
            "scenario schema must be 1",
        );
        let scenario_values = field_array(scenarios_value, "scenarios").unwrap_or(&[]);
        let mut by_id = BTreeMap::new();
        for scenario in scenario_values {
            let Some(id) = field_str(scenario, "id") else {
                self.errors.push("scenario is missing id".to_owned());
                continue;
            };
            self.require(!by_id.contains_key(id), "duplicate example scenario IDs");
            by_id.insert(id.to_owned(), scenario);
        }
        let scenario_ids: BTreeSet<&str> = by_id.keys().map(String::as_str).collect();

        for (scenario_id, scenario) in &by_id {
            let Some(fixture_path) = field_str(scenario, "fixture") else {
                self.errors
                    .push(format!("{scenario_id}: missing fixture path"));
                continue;
            };
            self.require(
                safe_relative_path(fixture_path),
                format!("{scenario_id}: unsafe fixture path"),
            );
            if let Some(fixture) = parsed.get(fixture_path) {
                self.require(
                    field_str(fixture, "component") == field_str(scenario, "component"),
                    format!("{scenario_id}: fixture component mismatch"),
                );
            } else {
                self.errors
                    .push(format!("{scenario_id}: missing fixture {fixture_path}"));
            }
            let timeout = field_i128(scenario, "timeout_ms").unwrap_or_default();
            self.require(
                timeout > 0 && timeout <= 120_000,
                format!("{scenario_id}: invalid timeout"),
            );
            let steps = field_array(scenario, "steps").unwrap_or(&[]);
            self.require(
                !steps.is_empty() && steps.len() <= 200,
                format!("{scenario_id}: invalid step count"),
            );
            let valid_viewport = field_object(scenario, "viewport")
                .and_then(|viewport| {
                    Some(
                        viewport.get("width")?.as_i128()?.gt(&0)
                            && viewport.get("width")?.as_i128()? <= 8192
                            && viewport.get("height")?.as_i128()?.gt(&0)
                            && viewport.get("height")?.as_i128()? <= 8192,
                    )
                })
                .unwrap_or(false);
            self.require(valid_viewport, format!("{scenario_id}: invalid viewport"));
            let clock = field_str(scenario, "clock");
            if clock == Some("fixed") {
                self.require(
                    field_str(scenario, "clock_at").is_some_and(has_timezone),
                    format!("{scenario_id}: clock_at needs timezone"),
                );
            } else {
                self.require(
                    clock == Some("real") && field(scenario, "clock_at").is_none(),
                    format!("{scenario_id}: invalid clock"),
                );
            }

            let mut step_ids = BTreeSet::new();
            for step in steps {
                let Some(step_id) = field_str(step, "id") else {
                    self.errors
                        .push(format!("{scenario_id}: step is missing id"));
                    continue;
                };
                self.require(
                    step_ids.insert(step_id.to_owned()),
                    format!("{scenario_id}: duplicate step IDs"),
                );
                let step_type = field_str(step, "type");
                let assertion = field_str(step, "assertion");
                let valid_assertion = field(step, "assertion").is_none()
                    || matches!(assertion, Some("no_runtime_errors" | "screenshot_matches"));
                self.require(
                    matches!(
                        step_type,
                        Some(
                            "click"
                                | "type_text"
                                | "key"
                                | "scroll"
                                | "assert"
                                | "wait_for"
                                | "capture"
                        )
                    ),
                    format!("{scenario_id}/{step_id}: invalid step type"),
                );
                if action_type(step_type) || !valid_assertion {
                    self.require(
                        field(step, "selector").is_some(),
                        format!("{scenario_id}/{step_id}: selector required"),
                    );
                }
                if let Some(selector) = field_object(step, "selector") {
                    let keys: BTreeSet<&str> = selector.keys().map(String::as_str).collect();
                    self.require(
                        keys == BTreeSet::from(["logical_id"])
                            || keys == BTreeSet::from(["name", "role"]),
                        format!("{scenario_id}/{step_id}: invalid selector"),
                    );
                } else if field(step, "selector").is_some() {
                    self.errors
                        .push(format!("{scenario_id}/{step_id}: invalid selector"));
                }
                if field(step, "timeout_ms").is_some() {
                    self.require(
                        field_i128(step, "timeout_ms")
                            .is_some_and(|value| value > 0 && value <= timeout),
                        format!("{scenario_id}/{step_id}: invalid step deadline"),
                    );
                }
                if field(step, "duration_ms").is_some() {
                    self.require(
                        step_type == Some("scroll")
                            && field_i128(step, "duration_ms")
                                .is_some_and(|value| value >= 0 && value <= timeout),
                        format!("{scenario_id}/{step_id}: invalid scroll duration"),
                    );
                }
                if assertion == Some("text_equals") {
                    self.require(
                        field(step, "expected").and_then(Value::as_str).is_some(),
                        format!("{scenario_id}/{step_id}: text expected must be a string"),
                    );
                }
            }
        }

        let Some(matrix) = parsed.get("matrix.toml") else {
            self.errors.push("missing matrix.toml".to_owned());
            return;
        };
        self.require(
            field_i128(matrix, "schema_version") == Some(1)
                && field_str(matrix, "source_mode") == Some("frozen"),
            "matrix needs v1 frozen inputs",
        );
        self.require(
            field_i128(matrix, "max_parallel").is_some_and(|value| value > 0),
            "matrix parallelism must be positive",
        );
        let targets = field_array(matrix, "targets").unwrap_or(&[]);
        let mut target_ids = BTreeSet::new();
        for target in targets {
            let Some(target_id) = field_str(target, "id") else {
                self.errors.push("matrix target is missing id".to_owned());
                continue;
            };
            self.require(
                target_ids.insert(target_id.to_owned()),
                "duplicate matrix target IDs",
            );
            let scenarios = field_array(target, "scenarios").unwrap_or(&[]);
            let scenarios_valid = !scenarios.is_empty()
                && scenarios.iter().all(|scenario| {
                    scenario
                        .as_str()
                        .is_some_and(|scenario| scenario_ids.contains(scenario))
                });
            self.require(
                scenarios_valid,
                format!("matrix target {target_id}: unknown/empty scenarios"),
            );
            self.require(
                field_i128(target, "timeout_ms").is_some_and(|value| value > 0)
                    && field_bool(target, "required").is_some(),
                format!("matrix target {target_id}: invalid deadline/required"),
            );
            let platform = field_str(target, "platform");
            self.require(
                matches!(
                    platform,
                    Some("macos" | "ios" | "android" | "windows" | "linux")
                ),
                format!("matrix target {target_id}: invalid platform"),
            );
            if matches!(platform, Some("ios" | "android")) {
                self.require(
                    field_str(target, "device").is_some_and(|device| !device.is_empty()),
                    format!("matrix target {target_id}: explicit device required"),
                );
            }
            if platform == Some("android") {
                self.require(
                    matches!(
                        field_str(target, "abi"),
                        Some("arm64-v8a" | "armeabi-v7a" | "x86" | "x86_64")
                    ),
                    format!("matrix target {target_id}: invalid ABI"),
                );
            }
        }

        let Some(perf) = parsed.get("performance-budget.toml") else {
            self.errors
                .push("missing performance-budget.toml".to_owned());
            return;
        };
        self.require(
            field_i128(perf, "schema_version") == Some(1)
                && field_str(perf, "algorithm") == Some("paired-bootstrap-v1"),
            "invalid perf schema/algorithm",
        );
        if let Some(sampling) = field(perf, "sampling") {
            self.require(
                field_i128(sampling, "warmup_runs").is_some_and(|value| value >= 10)
                    && field_i128(sampling, "measurement_runs").is_some_and(|value| value >= 30),
                "insufficient example perf runs",
            );
            self.require(
                path_f64(sampling, &["confidence"]) == Some(0.95)
                    && field_i128(sampling, "bootstrap_resamples")
                        .is_some_and(|value| value >= 2000),
                "invalid perf confidence/resamples",
            );
        } else {
            self.errors.push("missing performance sampling".to_owned());
        }
        for budget in field_array(perf, "budgets").unwrap_or(&[]) {
            let scenario_id = field_str(budget, "scenario").unwrap_or("");
            let Some(scenario) = by_id.get(scenario_id) else {
                self.errors.push(format!(
                    "performance budget references unknown scenario: {scenario_id}"
                ));
                continue;
            };
            let action_steps: BTreeSet<&str> = field_array(scenario, "steps")
                .unwrap_or(&[])
                .iter()
                .filter(|step| action_type(field_str(step, "type")))
                .filter_map(|step| field_str(step, "id"))
                .collect();
            let measure_steps = field_array(budget, "measure_steps").unwrap_or(&[]);
            let measure_steps_valid = !measure_steps.is_empty()
                && measure_steps.iter().all(|step| {
                    step.as_str()
                        .is_some_and(|step| action_steps.contains(step))
                });
            self.require(
                measure_steps_valid,
                "perf measurement references non-action/unknown step",
            );
            self.require(
                field_i128(budget, "min_samples").is_some_and(|value| value > 0)
                    && field_str(budget, "aggregate") == Some("median"),
                "invalid perf sample count/aggregate",
            );
            self.require(
                matches!(field_str(budget, "per_run"), Some("p95" | "peak"))
                    && field_str(budget, "direction") == Some("lower_is_better"),
                "invalid perf statistic/direction",
            );
            let absolute_enabled =
                field_bool(field(budget, "absolute").unwrap_or(&Value::Null), "enabled")
                    .unwrap_or(false);
            let relative_enabled =
                field_bool(field(budget, "relative").unwrap_or(&Value::Null), "enabled")
                    .unwrap_or(false);
            self.require(
                absolute_enabled || relative_enabled,
                "perf needs an enabled rule",
            );
        }

        let Some(observation) = parsed.get("observation-v2.json") else {
            self.errors.push("missing observation-v2.json".to_owned());
            return;
        };
        let observation_body = path(observation, &["result", "observation"]);
        self.require(
            field_i128(observation, "schema_version") == Some(2)
                && field_bool(observation, "ok") == Some(true),
            "invalid observation envelope example",
        );
        self.require(
            path_str(observation, &["result", "operation", "state"]) == Some("succeeded"),
            "example observation operation must be complete",
        );
        self.require(
            field_str(observation, "session_id")
                == observation_body.and_then(|value| field_str(value, "session_id")),
            "observation session mismatch",
        );
        let epochs = [
            observation_body.and_then(|value| field(value, "scene_epoch")),
            observation_body.and_then(|value| path(value, &["semantics", "scene_epoch"])),
            observation_body.and_then(|value| path(value, &["screenshot", "scene_epoch_before"])),
            observation_body.and_then(|value| path(value, &["screenshot", "scene_epoch_after"])),
        ];
        let same_epoch = epochs.first().and_then(|epoch| *epoch).is_some()
            && epochs.windows(2).all(|pair| pair[0] == pair[1]);
        self.require(same_epoch, "same_scene example has inconsistent epochs");
        self.require(
            observation_body.is_some_and(|value| {
                matches!(field(value, "presented_frame_id"), Some(Value::Null))
                    && path_str(value, &["check", "status"]) == Some("not_run")
            }),
            "observation example must not imply presentation/assertion proof",
        );
        if let Some(screenshot) = observation_body.and_then(|value| field(value, "screenshot")) {
            let size_bytes = field_i128(screenshot, "size_bytes");
            let pixel_width = field_i128(screenshot, "pixel_width");
            let pixel_height = field_i128(screenshot, "pixel_height");
            let logical_width = field_i128(screenshot, "logical_width");
            let logical_height = field_i128(screenshot, "logical_height");
            let scale = field(screenshot, "scale").and_then(Value::as_f64);
            self.require(
                size_bytes.is_some_and(|value| value <= 16 * 1024 * 1024)
                    && pixel_width
                        .zip(pixel_height)
                        .is_some_and(|(width, height)| width * height <= 20_000_000),
                "example image exceeds quota",
            );
            let scale_matches = match (
                pixel_width,
                pixel_height,
                logical_width,
                logical_height,
                scale,
            ) {
                (
                    Some(pixel_width),
                    Some(pixel_height),
                    Some(logical_width),
                    Some(logical_height),
                    Some(scale),
                ) => {
                    (pixel_width as f64 - logical_width as f64 * scale).abs() < f64::EPSILON
                        && (pixel_height as f64 - logical_height as f64 * scale).abs()
                            < f64::EPSILON
                }
                _ => false,
            };
            self.require(scale_matches, "example screenshot scale mismatch");
        } else {
            self.errors
                .push("observation example is missing screenshot".to_owned());
        }

        for name in ["repro-manifest.json", "template-manifest.json"] {
            let Some(manifest) = parsed.get(name) else {
                self.errors.push(format!("missing {name}"));
                continue;
            };
            self.require(
                field_i128(manifest, "schema_version") == Some(1),
                format!("{name}: schema must be 1"),
            );
            let mut normalized = BTreeSet::new();
            let mut valid_paths = true;
            for entry in field_array(manifest, "files").unwrap_or(&[]) {
                let Some(file_path) = field_str(entry, "path") else {
                    valid_paths = false;
                    continue;
                };
                valid_paths &= safe_relative_path(file_path);
                normalized.insert(lowercase_key(file_path));
            }
            self.require(valid_paths, format!("{name}: unsafe path"));
            let path_count = field_array(manifest, "files").unwrap_or(&[]).len();
            self.require(
                path_count == normalized.len(),
                format!("{name}: colliding paths"),
            );
        }

        if let Some(repro) = parsed.get("repro-manifest.json") {
            let file_entries = field_array(repro, "files").unwrap_or(&[]);
            let file_paths: BTreeSet<String> = file_entries
                .iter()
                .filter_map(|entry| field_str(entry, "path").map(str::to_owned))
                .collect();
            self.require(
                field_str(repro, "replayability") == Some("requires_source")
                    && path(repro, &["source", "included"]).and_then(Value::as_bool) == Some(false),
                "source-free repro must require source",
            );
            self.require(
                path_str(repro, &["source", "manifest_path"])
                    .is_some_and(|path| file_paths.contains(path))
                    && path_str(repro, &["failure", "result_path"])
                        .is_some_and(|path| file_paths.contains(path)),
                "repro references missing manifest/result",
            );
            let redacted_files: BTreeSet<String> = path(repro, &["privacy", "redacted_files"])
                .and_then(Value::as_array)
                .unwrap_or(&[])
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let expected_redacted: BTreeSet<String> = file_entries
                .iter()
                .filter(|entry| field_bool(entry, "redacted") == Some(true))
                .filter_map(|entry| field_str(entry, "path").map(str::to_owned))
                .collect();
            self.require(
                redacted_files == expected_redacted,
                "repro redaction inventory differs",
            );
            if let Some(failure_scenario) =
                path_str(repro, &["failure", "scenario_id"]).and_then(|id| by_id.get(id).copied())
            {
                let failure_step = path_str(repro, &["failure", "step_id"]);
                let known_steps: BTreeSet<&str> = field_array(failure_scenario, "steps")
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|step| field_str(step, "id"))
                    .collect();
                self.require(
                    failure_step.is_some_and(|step| known_steps.contains(step)),
                    "repro failure references missing step",
                );
            } else {
                self.errors
                    .push("repro failure references missing scenario".to_owned());
            }
        }

        if let Some(template) = parsed.get("template-manifest.json") {
            let groups: BTreeSet<&str> = field_array(template, "groups")
                .unwrap_or(&[])
                .iter()
                .filter_map(|group| field_str(group, "id"))
                .collect();
            let all_groups_known = field_array(template, "files")
                .unwrap_or(&[])
                .iter()
                .all(|entry| field_str(entry, "group").is_some_and(|group| groups.contains(group)));
            self.require(all_groups_known, "template references missing atomic group");
        }

        let Some(doctor) = parsed.get("doctor-v2.json") else {
            self.errors.push("missing doctor-v2.json".to_owned());
            return;
        };
        self.require(
            field_i128(doctor, "schema_version") == Some(2),
            "doctor schema must be 2",
        );
        let required_pass = field_array(doctor, "checks")
            .unwrap_or(&[])
            .iter()
            .filter(|check| field_bool(check, "required") == Some(true))
            .all(|check| field_str(check, "status") == Some("pass"));
        self.require(
            (field_str(doctor, "overall") == Some("pass")) == required_pass,
            "doctor overall disagrees with required checks",
        );
    }
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let character = trimmed.chars().next()?;
    if !matches!(character, '`' | '~') {
        return None;
    }
    let length = trimmed
        .chars()
        .take_while(|item| *item == character)
        .count();
    (length >= 3).then_some((character, length))
}

pub fn run() -> Result<(), String> {
    let mut validator = Validator::new(workspace_root());
    let mut documents = Vec::new();
    let mut document_paths = vec![
        validator.root.join("README.md"),
        validator.root.join("CONTRIBUTING.md"),
    ];
    collect_files(&validator.docs, "md", &mut document_paths)?;
    document_paths.sort();
    for path in document_paths {
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", validator.display_path(&path)))?;
        validator.links(&path, &text);
        documents.push((path, text));
    }
    let (task_count, case_count) = validator.indexes(&documents);

    let mut example_paths = Vec::new();
    collect_files(&validator.examples, "json", &mut example_paths)?;
    collect_files(&validator.examples, "toml", &mut example_paths)?;
    example_paths.sort();
    let mut parsed = BTreeMap::new();
    for path in example_paths {
        let name = path
            .strip_prefix(&validator.examples)
            .map_err(|error| format!("cannot relativize {}: {error}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", validator.display_path(&path)))?;
        let value = match path.extension().and_then(|value| value.to_str()) {
            Some("json") => parse_json(&text),
            Some("toml") => toml::from_str::<toml::Value>(&text)
                .map(from_toml)
                .map_err(|error| error.to_string()),
            _ => continue,
        };
        match value {
            Ok(value) => {
                validator.hashes(&value, &name);
                parsed.insert(name, value);
            }
            Err(error) => validator.errors.push(format!("{name}: {error}")),
        }
    }
    validator.examples(&parsed);

    if !validator.errors.is_empty() {
        for error in &validator.errors {
            eprintln!("ERROR: {error}");
        }
        return Err(format!(
            "design-document check failed with {} error(s)",
            validator.errors.len()
        ));
    }

    println!(
        "OK: {} Markdown files, {} local links, {} tasks (acyclic), {} acceptance cases, {} JSON/TOML examples.",
        documents.len(),
        validator.link_count,
        task_count,
        case_count,
        parsed.len()
    );
    println!("Draft consistency only; no runtime/GPU tests or real artifact hash validation.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Value, parse_json, safe_relative_path};

    #[test]
    fn rejects_duplicate_json_keys_at_any_depth() {
        let error =
            parse_json(r#"{"outer":{"value":1,"value":2}}"#).expect_err("duplicate key must fail");
        assert!(error.contains("duplicate JSON key: value"));
    }

    #[test]
    fn accepts_only_safe_relative_paths() {
        assert!(safe_relative_path("fixtures/counter-zero.json"));
        assert!(!safe_relative_path("../outside.json"));
        assert!(!safe_relative_path("fixtures//counter.json"));
        assert!(!safe_relative_path("fixtures\\counter.json"));
        assert!(!safe_relative_path("C:/counter.json"));
    }

    #[test]
    fn parses_json_scalars_and_objects() {
        let value = parse_json(r#"{"ok":true,"items":[1,null,"text"]}"#).expect("valid JSON");
        assert!(matches!(value, Value::Object(_)));
    }
}
