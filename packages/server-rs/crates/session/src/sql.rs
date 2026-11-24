use opencode_bus::BusError;
use rusqlite::Connection;
use rusqlite::types::Value;

pub(crate) enum Assignment {
    Value(&'static str, Value),
    Keep(&'static str),
}

pub(crate) struct TokenTotals {
    pub input: f64,
    pub output: f64,
    pub reasoning: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

pub(crate) fn set_session(
    connection: &Connection,
    session_id: &str,
    assignments: &[Assignment],
) -> Result<(), BusError> {
    let mut values = Vec::with_capacity(assignments.len() + 1);
    let mut assignments_sql = Vec::with_capacity(assignments.len());
    for assignment in assignments {
        match assignment {
            Assignment::Value(column, value) => {
                values.push(value.clone());
                assignments_sql.push(format!("{column} = ?{}", values.len()));
            }
            Assignment::Keep(column) => assignments_sql.push(format!("{column} = {column}")),
        }
    }
    let id = values.len() + 1;
    values.push(Value::Text(session_id.to_string()));
    connection.execute(
        &format!("UPDATE session_v2 SET {} WHERE id = ?{id}", assignments_sql.join(", ")),
        rusqlite::params_from_iter(values),
    )?;
    Ok(())
}

pub(crate) fn number(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.2e18 {
        return Value::Integer(value as i64);
    }
    Value::Real(value)
}

pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |duration| duration.as_millis() as i64)
}

pub(crate) fn apply_usage(
    connection: &Connection,
    session_id: &str,
    cost: f64,
    tokens: &TokenTotals,
) -> Result<(), BusError> {
    connection.execute(
        "UPDATE session_v2 SET cost = cost + ?1, tokens_input = tokens_input + ?2, \
         tokens_output = tokens_output + ?3, tokens_reasoning = tokens_reasoning + ?4, \
         tokens_cache_read = tokens_cache_read + ?5, tokens_cache_write = tokens_cache_write + ?6, \
         time_updated = time_updated WHERE id = ?7",
        rusqlite::params_from_iter([
            number(cost),
            number(tokens.input),
            number(tokens.output),
            number(tokens.reasoning),
            number(tokens.cache_read),
            number(tokens.cache_write),
            Value::Text(session_id.to_string()),
        ]),
    )?;
    Ok(())
}
