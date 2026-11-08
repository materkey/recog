//! Python's dict and list operations over [`Value`], failing as Python fails: `d[k]`
//! on a missing key is a `KeyError`, `.get` on a non-dict an `AttributeError`, and so
//! on. The storage's `except` clauses catch some of these and not others, so which
//! one a malformed file raises decides what happens next.

use std::cmp::Ordering;

use crate::py;
use crate::pyjson::{Map, Value};

use super::StorageError;

pub fn attribute_error(value: &Value, attribute: &str) -> StorageError {
    StorageError::Attribute(format!("'{}' object has no attribute '{attribute}'", value.type_name()))
}

fn not_subscriptable(value: &Value) -> StorageError {
    match value {
        Value::Array(_) => StorageError::Type("list indices must be integers or slices, not str".into()),
        Value::Str(_) => StorageError::Type("string indices must be integers, not 'str'".into()),
        other => StorageError::Type(format!("'{}' object is not subscriptable", other.type_name())),
    }
}

/// `value.get(key)`.
pub fn get<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>, StorageError> {
    value.as_object().map(|m| m.get(key)).ok_or_else(|| attribute_error(value, "get"))
}

/// `value.get(key, default)` where the default is `{}`: a missing key reads as an
/// empty dict.
pub fn get_or_empty<'a>(value: &'a Value, key: &str, empty: &'a Value) -> Result<&'a Value, StorageError> {
    Ok(get(value, key)?.unwrap_or(empty))
}

/// `value[key]`.
pub fn item<'a>(value: &'a Value, key: &str) -> Result<&'a Value, StorageError> {
    match value {
        Value::Object(m) => m.get(key).ok_or_else(|| StorageError::Key(py::repr(key))),
        other => Err(not_subscriptable(other)),
    }
}

/// `value[key]` for assignment through it.
pub fn item_mut<'a>(value: &'a mut Value, key: &str) -> Result<&'a mut Value, StorageError> {
    match value {
        Value::Object(m) => m.get_mut(key).ok_or_else(|| StorageError::Key(py::repr(key))),
        other => Err(not_subscriptable(other)),
    }
}

/// `value[key] = new`.
pub fn set(value: &mut Value, key: &str, new: Value) -> Result<(), StorageError> {
    match value {
        Value::Object(m) => {
            m.insert(key, new);
            Ok(())
        }
        Value::Array(_) => Err(StorageError::Type("list indices must be integers or slices, not str".into())),
        Value::Str(_) => Err(StorageError::Type("'str' object does not support item assignment".into())),
        other => Err(StorageError::Type(format!("'{}' object does not support item assignment", other.type_name()))),
    }
}

/// The dict behind `value.<method>()`, or the `AttributeError` that call raises.
pub fn items<'a>(value: &'a Value, method: &str) -> Result<&'a Map, StorageError> {
    value.as_object().ok_or_else(|| attribute_error(value, method))
}

/// `key in value`: a dict's keys, a list's items, a string's substrings.
pub fn contains(value: &Value, key: &str) -> Result<bool, StorageError> {
    match value {
        Value::Object(m) => Ok(m.contains_key(key)),
        Value::Array(list) => Ok(list.iter().any(|v| v.as_str() == Some(key))),
        Value::Str(s) => Ok(s.contains(key)),
        other => Err(StorageError::Type(format!("argument of type '{}' is not iterable", other.type_name()))),
    }
}

/// The number a comparison or arithmetic sees, for int, float and bool.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        #[allow(clippy::cast_precision_loss)]
        Value::Int(n) => Some(*n as f64),
        Value::Float(x) => Some(*x),
        _ => None,
    }
}

/// `value > n`, `value == n`, … against an int.
pub fn compare_int(value: &Value, n: i64, op: &str) -> Result<Ordering, StorageError> {
    let x = number(value).ok_or_else(|| {
        StorageError::Type(format!("'{op}' not supported between instances of '{}' and 'int'", value.type_name()))
    })?;
    #[allow(clippy::cast_precision_loss)]
    Ok(x.partial_cmp(&(n as f64)).unwrap_or(Ordering::Less))
}

/// `value == n` as Python decides it, never failing.
pub fn equals_int(value: &Value, n: i64) -> bool {
    #[allow(clippy::cast_precision_loss)]
    number(value).is_some_and(|x| x == n as f64)
}

/// `value + n` for a number already stored.
pub fn add_int(value: &Value, n: i64) -> Result<Value, StorageError> {
    match value {
        Value::Int(m) => Ok(Value::Int(m + i128::from(n))),
        Value::Bool(b) => Ok(Value::Int(i128::from(*b) + i128::from(n))),
        #[allow(clippy::cast_precision_loss)]
        Value::Float(x) => Ok(Value::Float(x + n as f64)),
        other => {
            Err(StorageError::Type(format!("unsupported operand type(s) for +=: '{}' and 'int'", other.type_name())))
        }
    }
}

/// `int(value)`.
pub fn to_int(value: &Value) -> Result<i64, StorageError> {
    match value {
        Value::Int(n) => i64::try_from(*n).map_err(|_| StorageError::Value("integer out of range".into())),
        Value::Bool(b) => Ok(i64::from(*b)),
        #[allow(clippy::cast_possible_truncation)]
        Value::Float(x) if x.is_finite() => Ok(x.trunc() as i64),
        Value::Float(x) if x.is_nan() => Err(StorageError::Value("cannot convert float NaN to integer".into())),
        Value::Float(_) => Err(StorageError::Value("cannot convert float infinity to integer".into())),
        Value::Str(s) => py::parse_int(s).map_err(StorageError::Value),
        other => Err(StorageError::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        ))),
    }
}

/// Python's `<` between two sort keys: strings by code point, numbers by value.
pub fn compare_keys(a: &Value, b: &Value) -> Result<Ordering, StorageError> {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        _ => match (number(a), number(b)) {
            (Some(x), Some(y)) => Ok(x.partial_cmp(&y).unwrap_or(Ordering::Equal)),
            _ => Err(StorageError::Type(format!(
                "'<' not supported between instances of '{}' and '{}'",
                a.type_name(),
                b.type_name()
            ))),
        },
    }
}

/// `items.sort(key=key, reverse=True)`: stable, failing as Python does on keys it
/// cannot compare.
pub fn sort_desc<T>(items: &mut Vec<T>, key: impl Fn(&T) -> Result<Value, StorageError>) -> Result<(), StorageError> {
    let keys = items.iter().map(&key).collect::<Result<Vec<_>, _>>()?;
    let mut error = None;
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&i, &j| match compare_keys(&keys[j], &keys[i]) {
        Ok(o) => o,
        Err(e) => {
            error.get_or_insert(e);
            Ordering::Equal
        }
    });
    if let Some(e) = error {
        return Err(e);
    }
    let mut slots: Vec<Option<T>> = items.drain(..).map(Some).collect();
    items.extend(order.into_iter().filter_map(|i| slots[i].take()));
    Ok(())
}

/// `", ".join(values)`, which wants strings only.
pub fn join_strings(values: &[Value]) -> Result<String, StorageError> {
    let mut parts = Vec::with_capacity(values.len());
    for (i, value) in values.iter().enumerate() {
        match value {
            Value::Str(s) => parts.push(s.as_str()),
            other => {
                return Err(StorageError::Type(format!(
                    "sequence item {i}: expected str instance, {} found",
                    other.type_name()
                )));
            }
        }
    }
    Ok(parts.join(", "))
}

/// `'%g' % x`.
#[must_use]
pub fn format_g(x: f64) -> String {
    if !x.is_finite() {
        return py::float_repr(x);
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let scientific = format!("{x:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let trim = |s: String| {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
    };
    if (-4..6).contains(&exponent) {
        let decimals = usize::try_from(5 - exponent).unwrap_or(0);
        trim(format!("{x:.decimals$}"))
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa.to_owned()), exponent.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_g_formats_like_python() {
        // given / when / then
        for (x, text) in
            [(24.0, "24"), (0.5, "0.5"), (1e-5, "1e-05"), (123_456_789.0, "1.23457e+08"), (0.0001, "0.0001")]
        {
            assert_eq!(format_g(x), text);
        }
    }

    #[test]
    fn descending_sort_is_stable_and_refuses_mixed_keys() {
        // given
        let mut items = vec![("b", 1), ("a", 2), ("b", 3)];
        // when
        sort_desc(&mut items, |(k, _)| Ok(Value::from(*k))).unwrap();
        // then
        assert_eq!(items, vec![("b", 1), ("b", 3), ("a", 2)]);
        let mut mixed = vec![Value::Null, Value::from("x")];
        assert!(matches!(sort_desc(&mut mixed, |v| Ok(v.clone())), Err(StorageError::Type(_))));
    }
}
