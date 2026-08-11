//! Argument extraction, in the same words `tools/fs` and `tools/tasks` use.
//!
//! A near-copy of `tools/fs/src/args.rs`, and deliberately a copy rather than a
//! shared crate — the same call `tools/tasks` made. `args` is private in both,
//! the whole of it is thirty lines of `match`, and the property that matters is
//! that the *messages* are the same shape, not that the code is. Making it
//! public to save this file would export a module whose only job is producing
//! one crate's error prose.
//!
//! Two conventions carried across because the model depends on them: an
//! explicit JSON `null` is treated as absent, and everything here is pure so it
//! can run in `validate_args` before any process is started.

use emma_tool_api::ToolError;
use serde_json::Value;

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError::BadArguments(msg.into())
}

fn object<'a>(
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

/// Unknown keys are refused rather than ignored: a silently dropped
/// `occurrence` looks to the model exactly like a parameter that had no effect,
/// and it concludes the behaviour is impossible rather than that it misspelled
/// the key.
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
    match object(args, tool)?.get(key) {
        None => Err(bad(format!("{tool} requires {key}"))),
        Some(Value::String(s)) => Ok(s),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be a string, got {}",
            type_name(other)
        ))),
    }
}

pub fn req_u64(args: &Value, tool: &str, key: &str) -> Result<u64, ToolError> {
    match object(args, tool)?.get(key) {
        None => Err(bad(format!("{tool} requires {key}"))),
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| {
            bad(format!(
                "{tool}.{key} must be a non-negative whole number, got {n}"
            ))
        }),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be a number, got {}",
            type_name(other)
        ))),
    }
}

pub fn opt_u64(args: &Value, tool: &str, key: &str) -> Result<Option<u64>, ToolError> {
    match object(args, tool)?.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n.as_u64().map(Some).ok_or_else(|| {
            bad(format!(
                "{tool}.{key} must be a non-negative whole number, got {n}"
            ))
        }),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be a number, got {}",
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
