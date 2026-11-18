//! The JSON Schema subset the tool specs use (Draft 2020-12 semantics):
//! type, properties, required, additionalProperties=false, enum, minimum,
//! minItems, maxItems, maxLength, items.
use serde_json::Value as Json;

fn type_ok(t: &str, v: &Json) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "number" => v.is_number(),
        // 2020-12: 3.0 is an integer.
        "integer" => {
            v.as_i64().is_some()
                || v.as_u64().is_some()
                || v.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        "null" => v.is_null(),
        _ => true,
    }
}

pub fn validate(schema: &Json, v: &Json) -> Result<(), String> {
    check(schema, v, "$")
}

fn check(s: &Json, v: &Json, at: &str) -> Result<(), String> {
    if let Some(t) = s.get("type").and_then(Json::as_str) {
        if !type_ok(t, v) {
            return Err(format!("{v} is not of type '{t}' at {at}"));
        }
    }
    if let Some(e) = s.get("enum").and_then(Json::as_array) {
        if !e.contains(v) {
            return Err(format!(
                "{v} is not one of {} at {at}",
                Json::Array(e.clone())
            ));
        }
    }
    if let (Some(min), Some(x)) = (s.get("minimum").and_then(Json::as_f64), v.as_f64()) {
        if x < min {
            return Err(format!("{v} is less than the minimum of {min} at {at}"));
        }
    }
    if let (Some(max), Some(x)) = (s.get("maxLength").and_then(Json::as_u64), v.as_str()) {
        if x.chars().count() as u64 > max {
            return Err(format!("string is longer than {max} characters at {at}"));
        }
    }
    if let Some(items) = v.as_array() {
        if let Some(min) = s.get("minItems").and_then(Json::as_u64) {
            if (items.len() as u64) < min {
                return Err(format!("array should have at least {min} items at {at}"));
            }
        }
        if let Some(max) = s.get("maxItems").and_then(Json::as_u64) {
            if items.len() as u64 > max {
                return Err(format!("array has more than {max} items at {at}"));
            }
        }
        if let Some(item) = s.get("items") {
            for (i, x) in items.iter().enumerate() {
                check(item, x, &format!("{at}[{i}]"))?;
            }
        }
    }
    if let Some(obj) = v.as_object() {
        let props = s.get("properties").and_then(Json::as_object);
        for r in s
            .get("required")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
        {
            if !obj.contains_key(r) {
                return Err(format!("'{r}' is a required property at {at}"));
            }
        }
        if s.get("additionalProperties") == Some(&Json::Bool(false)) {
            if let Some(k) = obj
                .keys()
                .find(|k| props.is_none_or(|p| !p.contains_key(*k)))
            {
                return Err(format!(
                    "Additional properties are not allowed ('{k}' was unexpected) at {at}"
                ));
            }
        }
        for (k, x) in obj {
            if let Some(ps) = props.and_then(|p| p.get(k)) {
                check(ps, x, &format!("{at}.{k}"))?;
            }
        }
    }
    Ok(())
}
