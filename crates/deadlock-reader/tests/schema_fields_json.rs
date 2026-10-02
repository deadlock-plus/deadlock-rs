//! The machine-readable field table and the compiled one are the same table.
//!
//! `schema-fields.json` ships so a tool outside this workspace can read the class/field
//! table without parsing Rust. It is only worth shipping while it agrees with
//! [`WIN64_FIELDS`], which a copy of a 74-row table does not do on its own.

use std::collections::BTreeMap;

use deadlock_reader::fields::WIN64_FIELDS;
use serde_json::Value;

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema-fields.json");

fn table() -> Value {
    let text = std::fs::read_to_string(PATH).unwrap_or_else(|e| panic!("read {PATH}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {PATH}: {e}"))
}

fn rows(v: &Value) -> Vec<(String, String, Option<u32>, bool)> {
    v["fields"]
        .as_array()
        .expect("`fields` must be an array")
        .iter()
        .map(|r| {
            let class = r["class"]
                .as_str()
                .expect("class must be a string")
                .to_string();
            let field = r["field"]
                .as_str()
                .expect("field must be a string")
                .to_string();
            let fallback = match &r["fallback"] {
                Value::Null => None,
                Value::String(s) => {
                    let hex = s.strip_prefix("0x").unwrap_or_else(|| {
                        panic!("{class}::{field} fallback {s} is not 0x-prefixed")
                    });
                    Some(
                        u32::from_str_radix(hex, 16)
                            .unwrap_or_else(|e| panic!("{class}::{field} fallback {s}: {e}")),
                    )
                }
                other => {
                    panic!("{class}::{field} fallback must be a hex string or null, got {other}")
                }
            };
            let inferred = r["inferred"]
                .as_bool()
                .unwrap_or_else(|| panic!("{class}::{field} inferred must be a bool"));
            (class, field, fallback, inferred)
        })
        .collect()
}

#[test]
fn the_json_field_table_lists_exactly_what_the_crate_compiles() {
    let json = table();
    assert_eq!(
        json["version"].as_u64(),
        Some(1),
        "table version changed without the test"
    );

    let compiled: BTreeMap<(&str, &str), (Option<u32>, bool)> = WIN64_FIELDS
        .iter()
        .map(|f| ((f.class, f.field), (f.fallback, f.inferred)))
        .collect();
    let parsed = rows(&json);
    let shipped: BTreeMap<(&str, &str), (Option<u32>, bool)> = parsed
        .iter()
        .map(|(c, f, fb, inf)| ((c.as_str(), f.as_str()), (*fb, *inf)))
        .collect();

    let only_json: Vec<_> = shipped
        .keys()
        .filter(|k| !compiled.contains_key(*k))
        .collect();
    let only_rust: Vec<_> = compiled
        .keys()
        .filter(|k| !shipped.contains_key(*k))
        .collect();
    assert!(
        only_json.is_empty() && only_rust.is_empty(),
        "rows only in schema-fields.json: {only_json:?}\nrows only in WIN64_FIELDS: {only_rust:?}"
    );

    let disagree: Vec<_> = compiled
        .iter()
        .filter(|(k, v)| shipped.get(*k) != Some(v))
        .map(|(k, v)| (k, v, shipped.get(k)))
        .collect();
    assert!(
        disagree.is_empty(),
        "rows whose fallback or inferred flag differ (key, WIN64_FIELDS, json): {disagree:?}"
    );
}

#[test]
fn the_json_field_table_is_sorted_and_free_of_duplicates() {
    let json = table();
    let parsed = rows(&json);
    let keys: Vec<(&str, &str)> = parsed
        .iter()
        .map(|(c, f, _, _)| (c.as_str(), f.as_str()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        keys, sorted,
        "schema-fields.json must be sorted by (class, field) with no duplicate pair"
    );
}
