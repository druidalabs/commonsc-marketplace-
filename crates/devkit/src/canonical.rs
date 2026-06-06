//! Canonical JSON encoding for hashing and signing.
//!
//! Produces deterministic bytes from any `serde_json::Value` by recursively
//! sorting object keys lexicographically. No insignificant whitespace, no
//! escape variations. Matches the canonicalization the customer app uses in
//! `ui/src/catalog.ts` so manifests signed by the dev-kit verify byte-for-byte
//! on the customer side.

use std::io::Write;

use serde_json::Value;

pub fn bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push(b'[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(out, v);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(out, k);
                out.push(b':');
                write_value(out, &map[*k]);
            }
            out.push(b'}');
        }
    }
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    // serde_json's string encoder matches the canonicalization the customer app
    // does via JSON.stringify(s): same escape set, no extras. Use it directly.
    let mut tmp = Vec::with_capacity(s.len() + 2);
    write!(&mut tmp, "{}", serde_json::Value::String(s.to_string())).unwrap();
    out.extend_from_slice(&tmp);
}
