//! The input schema each tool publishes, built from the `OPERATIONS` table so a
//! field an operation takes cannot be missing from it.

use std::collections::BTreeSet;
use std::sync::Arc;

use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::{Kind, OPERATIONS, READ_OPERATIONS, ToolKind, WRITE_OPERATIONS};

/// Fields every operation of a tool accepts, besides `operation` itself.
const COMMON_FIELDS: [(&str, Kind); 2] = [("profile", Kind::Text), ("workspace", Kind::Text)];

/// A few words for the fields whose name alone misleads.
fn hint(field: &str) -> Option<&'static str> {
    Some(match field {
        "repo" => "workspace/repo-slug",
        "state" => "open, merged, declined, superseded",
        "strategy" => "merge-commit, squash or fast-forward",
        "var" | "secured_var" => "KEY=VALUE",
        "wait" => "wait for the pipeline to finish; long call",
        "limit" => "results per page, max 100",
        _ => return None,
    })
}

fn property(field: &str, kind: Kind) -> Value {
    let mut property = match kind {
        Kind::Text => json!({"type": "string"}),
        Kind::Number => json!({"type": "integer"}),
        Kind::Switch => json!({"type": "boolean"}),
        Kind::Texts => json!({"type": "array", "items": {"type": "string"}}),
    };
    if let (Some(hint), Value::Object(map)) = (hint(field), &mut property) {
        map.insert("description".to_owned(), json!(hint));
    }
    property
}

/// The published input schema of a tool: `operation` narrowed to the tool's
/// operations, the common fields, and the union of the fields those operations take.
pub(super) fn input_schema(tool: ToolKind) -> Arc<JsonObject> {
    let operations = match tool {
        ToolKind::Read => READ_OPERATIONS,
        ToolKind::Write => WRITE_OPERATIONS,
    };
    let mut seen = BTreeSet::new();
    let mut properties = JsonObject::new();
    properties.insert(
        "operation".to_owned(),
        json!({"type": "string", "enum": operations}),
    );
    let table_fields = OPERATIONS
        .iter()
        .filter(|(name, _)| operations.contains(name))
        .flat_map(|(_, fields)| fields.iter())
        .filter_map(|field| field.name.map(|name| (name, field.kind)));
    for (name, kind) in COMMON_FIELDS.into_iter().chain(table_fields) {
        if seen.insert(name) {
            properties.insert(name.to_owned(), property(name, kind));
        }
    }
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), json!(["operation"]));
    schema.insert("additionalProperties".to_owned(), json!(false));
    Arc::new(schema)
}
