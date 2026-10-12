//! Chart drawings Aeris saved before Aeris Charts owned the drawings document.

use aeris_charts_engine::{DRAWINGS_DOCUMENT_SCHEMA, DRAWINGS_DOCUMENT_VERSION};
use serde_json::Value;

/// Saved chart drawings as an Aeris Charts drawings document.
///
/// Workspaces once saved the bare drawing array with each anchor's exchange time in
/// `aeris_anchor_time` and the locked drawings as a separate id list; that form is upgraded,
/// while a drawings document passes through unchanged and `legacy_locked_ids` is ignored.
///
/// # Errors
///
/// Returns an error when the saved drawings are not JSON.
pub fn saved_drawings_document(
    saved_json: &str,
    legacy_locked_ids: &[u32],
) -> Result<String, String> {
    let Value::Array(mut drawings) = serde_json::from_str::<Value>(saved_json)
        .map_err(|_| "saved drawings are malformed".to_string())?
    else {
        return Ok(saved_json.to_string());
    };
    for drawing in &mut drawings {
        let locked = drawing
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .is_some_and(|id| legacy_locked_ids.contains(&id));
        let Some(drawing) = drawing.as_object_mut() else {
            continue;
        };
        if locked {
            drawing.insert("locked".to_string(), Value::Bool(true));
        }
        let points = drawing.get_mut("points").and_then(Value::as_array_mut);
        for point in points
            .into_iter()
            .flatten()
            .filter_map(Value::as_object_mut)
        {
            if let Some(time) = point.remove("aeris_anchor_time") {
                point.insert("time".to_string(), time);
            }
        }
    }
    serde_json::to_string(&serde_json::json!({
        "schema": DRAWINGS_DOCUMENT_SCHEMA,
        "schema_version": DRAWINGS_DOCUMENT_VERSION,
        "drawings": drawings,
    }))
    .map_err(|_| "saved drawings could not be upgraded".to_string())
}

#[cfg(test)]
mod tests {
    use super::saved_drawings_document;
    use serde_json::{Value, json};

    #[test]
    fn legacy_drawings_carry_their_exchange_times_and_locks_into_the_document() {
        let legacy = json!([
            {"id": 3, "kind": "trend_line", "pane_index": 0, "points": [
                {"logical": 1.0, "price": 10.0, "aeris_anchor_time": 60.0},
                {"logical": 2.0, "price": 11.0}
            ]},
            {"id": 4, "kind": "horizontal_line", "pane_index": 0, "points": [
                {"logical": 3.0, "price": 12.0}
            ]}
        ]);
        let document: Value = serde_json::from_str(
            &saved_drawings_document(&legacy.to_string(), &[4]).expect("legacy upgrades"),
        )
        .expect("document is JSON");
        assert_eq!(document["schema"], "aeris_charts-drawings");
        assert_eq!(document["schema_version"], 1);
        let drawings = &document["drawings"];
        assert_eq!(
            drawings[0]["points"][0],
            json!({"logical": 1.0, "price": 10.0, "time": 60.0})
        );
        assert_eq!(
            drawings[0]["points"][1],
            json!({"logical": 2.0, "price": 11.0})
        );
        assert_eq!(drawings[0].get("locked"), None);
        assert_eq!(drawings[1]["locked"], true);
    }

    #[test]
    fn a_document_passes_through_and_malformed_drawings_are_rejected() {
        let document = r#"{"schema":"aeris_charts-drawings","schema_version":1,"drawings":[]}"#;
        assert_eq!(
            saved_drawings_document(document, &[1]).as_deref(),
            Ok(document)
        );
        assert!(saved_drawings_document("[x]", &[]).is_err());
    }
}
