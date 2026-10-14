//! Keeping unknown keys across a typed rewrite of a Rust-owned state file.
//!
//! A state file is read into typed structs and written back from them, so a key this build
//! doesn't know (one a later 2.x build added, then rolled back) would be dropped. Before a save,
//! [`merge_unknown`] copies such keys from the file as it was loaded into the freshly serialised
//! value. Every key the types know is always serialised (`null` when empty), so only unknown keys
//! are ever copied back.

use serde_json::Value;

/// Copies into `new` every object key of `old` that `new` lacks, recursing where both sides hold
/// an object. Arrays are matched element by element through `id`: an element of `new` merges
/// with the element of `old` that has the same id, and elements without one are left as they
/// are. Any other value in `new` wins.
pub(crate) fn merge_unknown(new: &mut Value, old: &Value, id: &dyn Fn(&Value) -> Option<String>) {
    match (new, old) {
        (Value::Object(new), Value::Object(old)) => {
            for (key, old_value) in old {
                match new.get_mut(key) {
                    Some(new_value) => merge_unknown(new_value, old_value, id),
                    None => {
                        new.insert(key.clone(), old_value.clone());
                    }
                }
            }
        }
        (Value::Array(new), Value::Array(old)) => {
            for element in new {
                let Some(key) = id(element) else { continue };
                if let Some(old_element) = old.iter().find(|o| id(o).as_deref() == Some(&key)) {
                    merge_unknown(element, old_element, id);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn by_id(v: &Value) -> Option<String> {
        v.get("id").and_then(Value::as_str).map(str::to_owned)
    }

    #[test]
    fn unknown_keys_come_back_at_every_depth() {
        let old = json!({
            "schema": 1,
            "later": {"a": 1},
            "items": [
                {"id": "x", "n": 1, "extra": true, "inner": {"k": 1, "kept": "yes"}},
                {"id": "gone", "extra": true},
            ],
        });
        let mut new = json!({
            "schema": 1,
            "items": [
                {"id": "x", "n": 2, "inner": {"k": 2}},
                {"id": "y", "n": 3},
            ],
        });
        merge_unknown(&mut new, &old, &by_id);
        assert_eq!(
            new,
            json!({
                "schema": 1,
                "items": [
                    {"id": "x", "n": 2, "inner": {"k": 2, "kept": "yes"}, "extra": true},
                    {"id": "y", "n": 3},
                ],
                "later": {"a": 1},
            })
        );
    }

    #[test]
    fn a_cleared_value_stays_cleared() {
        let old = json!({"breaker": {"count": 3, "later": 1}});
        let mut new = json!({"breaker": null});
        merge_unknown(&mut new, &old, &by_id);
        assert_eq!(new, json!({"breaker": null}));
    }
}
