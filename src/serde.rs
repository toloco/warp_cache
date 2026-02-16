//! Fast-path serialization for common Python primitives.
//!
//! Tagged binary format — avoids pickle for None, bool, int, float, str, bytes,
//! and flat tuples of these types.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyFloat, PyInt, PyNone, PyString, PyTuple};

const TAG_PICKLE: u8 = 0;
const TAG_NONE: u8 = 1;
const TAG_FALSE: u8 = 2;
const TAG_TRUE: u8 = 3;
const TAG_I64: u8 = 4;
const TAG_F64: u8 = 5;
const TAG_STR: u8 = 6;
const TAG_BYTES: u8 = 7;
const TAG_TUPLE: u8 = 8;

/// Serialize a Python object to our tagged binary format.
/// Returns `None` if the type is unsupported (caller should fall back to pickle).
pub fn serialize(py: Python, obj: &Bound<PyAny>) -> PyResult<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    if serialize_element(py, obj, &mut buf)? {
        Ok(Some(buf))
    } else {
        Ok(None)
    }
}

/// Deserialize from our tagged binary format.
/// Returns `None` if the tag is TAG_PICKLE (caller should use pickle.loads on the payload).
pub fn deserialize(py: Python, data: &[u8]) -> PyResult<Option<Py<PyAny>>> {
    if data.is_empty() {
        return Ok(None);
    }
    if data[0] == TAG_PICKLE {
        return Ok(None);
    }
    match deserialize_one(py, data)? {
        Some((obj, _consumed)) => Ok(Some(obj)),
        None => Ok(None),
    }
}

/// Prepend TAG_PICKLE to raw pickle bytes.
pub fn wrap_pickle(pickle_bytes: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + pickle_bytes.len());
    buf.push(TAG_PICKLE);
    buf.extend_from_slice(pickle_bytes);
    buf
}

/// Return the pickle payload (skip the TAG_PICKLE byte).
pub fn pickle_payload(data: &[u8]) -> &[u8] {
    &data[1..]
}

/// Serialize one element into `buf`. Returns `false` if unsupported.
fn serialize_element(_py: Python, obj: &Bound<PyAny>, buf: &mut Vec<u8>) -> PyResult<bool> {
    // None
    if obj.is_instance_of::<PyNone>() {
        buf.push(TAG_NONE);
        return Ok(true);
    }

    // bool before int (bool <: int in Python)
    if obj.is_instance_of::<PyBool>() {
        if obj.is_truthy()? {
            buf.push(TAG_TRUE);
        } else {
            buf.push(TAG_FALSE);
        }
        return Ok(true);
    }

    // int
    if obj.is_instance_of::<PyInt>() {
        if let Ok(v) = obj.extract::<i64>() {
            buf.push(TAG_I64);
            buf.extend_from_slice(&v.to_le_bytes());
            return Ok(true);
        }
        // Large int — fall back to pickle
        return Ok(false);
    }

    // float
    if obj.is_instance_of::<PyFloat>() {
        let v: f64 = obj.extract()?;
        buf.push(TAG_F64);
        buf.extend_from_slice(&v.to_le_bytes());
        return Ok(true);
    }

    // str
    if obj.is_instance_of::<PyString>() {
        let s = obj.cast::<PyString>()?.to_cow()?;
        let bytes = s.as_bytes();
        buf.push(TAG_STR);
        buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(bytes);
        return Ok(true);
    }

    // bytes
    if obj.is_instance_of::<PyBytes>() {
        let b = obj.cast::<PyBytes>()?.as_bytes();
        buf.push(TAG_BYTES);
        buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
        buf.extend_from_slice(b);
        return Ok(true);
    }

    // tuple (flat — only primitives inside)
    if obj.is_instance_of::<PyTuple>() {
        let tup = obj.cast::<PyTuple>()?;
        let len = tup.len();
        if len > 255 {
            return Ok(false);
        }
        // Reserve tag + count, then serialize elements
        let start = buf.len();
        buf.push(TAG_TUPLE);
        buf.push(len as u8);
        for item in tup.iter() {
            if !serialize_element(_py, &item, buf)? {
                // Unsupported element — revert and fall back
                buf.truncate(start);
                return Ok(false);
            }
        }
        return Ok(true);
    }

    Ok(false)
}

/// Deserialize one element from `data`. Returns `(value, bytes_consumed)`.
fn deserialize_one(py: Python, data: &[u8]) -> PyResult<Option<(Py<PyAny>, usize)>> {
    if data.is_empty() {
        return Ok(None);
    }

    match data[0] {
        TAG_NONE => Ok(Some((py.None(), 1))),

        TAG_FALSE => {
            let obj = false.into_pyobject(py)?.to_owned().into_any().unbind();
            Ok(Some((obj, 1)))
        }

        TAG_TRUE => {
            let obj = true.into_pyobject(py)?.to_owned().into_any().unbind();
            Ok(Some((obj, 1)))
        }

        TAG_I64 => {
            if data.len() < 9 {
                return Ok(None);
            }
            let v = i64::from_le_bytes(data[1..9].try_into().unwrap());
            Ok(Some((v.into_pyobject(py)?.into_any().unbind(), 9)))
        }

        TAG_F64 => {
            if data.len() < 9 {
                return Ok(None);
            }
            let v = f64::from_le_bytes(data[1..9].try_into().unwrap());
            Ok(Some((v.into_pyobject(py)?.into_any().unbind(), 9)))
        }

        TAG_STR => {
            if data.len() < 5 {
                return Ok(None);
            }
            let len = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
            if data.len() < 5 + len {
                return Ok(None);
            }
            let s = std::str::from_utf8(&data[5..5 + len])
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
            Ok(Some((PyString::new(py, s).into_any().unbind(), 5 + len)))
        }

        TAG_BYTES => {
            if data.len() < 5 {
                return Ok(None);
            }
            let len = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
            if data.len() < 5 + len {
                return Ok(None);
            }
            Ok(Some((
                PyBytes::new(py, &data[5..5 + len]).into_any().unbind(),
                5 + len,
            )))
        }

        TAG_TUPLE => {
            if data.len() < 2 {
                return Ok(None);
            }
            let count = data[1] as usize;
            let mut offset = 2usize;
            let mut elems: Vec<Py<PyAny>> = Vec::with_capacity(count);
            for _ in 0..count {
                match deserialize_one(py, &data[offset..])? {
                    Some((val, consumed)) => {
                        elems.push(val);
                        offset += consumed;
                    }
                    None => return Ok(None),
                }
            }
            let tup = PyTuple::new(py, elems)?;
            Ok(Some((tup.into_any().unbind(), offset)))
        }

        _ => Ok(None),
    }
}
