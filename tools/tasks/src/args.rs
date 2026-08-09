//! Argument extraction, worded to match `tools/fs`.
//!
//! Duplicated rather than shared because `emma-tools-fs` keeps its `args`
//! module private, and this crate does not own that crate. The wording is
//! copied deliberately: the model learns the shape of one argument error and
//! then reads every other one, and a second dialect of the same message costs
//! that for nothing. If a third tool crate appears, this belongs in
//! `tool-api`.

use serde_json::Value;

use emma_tool_api::ToolError;

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError::BadArguments(msg.into())
}

pub fn object<'a>(
    args: &'a Value,
    tool: &str,
) -> Result<&'a serde_json::Map<String, Value>, ToolError> {
    args.as_object().ok_or_else(|| {
        bad(format!(
            "{tool} takes a JSON object, got {}",
            type_name(args)
        ))
    })
}

pub fn deny_unknown(args: &Value, tool: &str, known: &[&str]) -> Result<(), ToolError> {
    let obj = object(args, tool)?;
    let mut unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !known.contains(k))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    Err(bad(format!(
        "{tool} does not take {}; accepted parameters are {}",
        unknown.join(", "),
        known.join(", ")
    )))
}

pub fn req_str<'a>(args: &'a Value, tool: &str, key: &str) -> Result<&'a str, ToolError> {
    let obj = object(args, tool)?;
    match obj.get(key) {
        None => Err(bad(format!("{tool} requires {key}"))),
        Some(Value::String(s)) => Ok(s),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be a string, got {}",
            type_name(other)
        ))),
    }
}

pub fn opt_str<'a>(args: &'a Value, tool: &str, key: &str) -> Result<Option<&'a str>, ToolError> {
    let obj = object(args, tool)?;
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be a string, got {}",
            type_name(other)
        ))),
    }
}

pub fn req_array<'a>(args: &'a Value, tool: &str, key: &str) -> Result<&'a Vec<Value>, ToolError> {
    let obj = object(args, tool)?;
    match obj.get(key) {
        None => Err(bad(format!("{tool} requires {key}"))),
        Some(Value::Array(a)) => Ok(a),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be an array, got {}",
            type_name(other)
        ))),
    }
}

pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
