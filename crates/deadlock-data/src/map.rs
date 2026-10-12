//! Entity lumps of the game's map archives.

use std::collections::BTreeMap;
use std::path::Path;

use source2::ext::VpkResourceKv3;
use source2::kv3;
use source2::resource::BlockKind;
use source2::vpk::Vpk;

use crate::error::{Error, Result};
use crate::vdata::vpk_error;

/// The entities of one map, as the map compiler stored them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapEntities {
    /// CRC32 of the entry as the archive directory records it. It changes whenever the
    /// map's entity lump changes, so a table baked from the lump can detect a stale copy.
    pub crc: u32,
    /// One map per entity, in file order: each key as written in the file, each value as a
    /// string.
    ///
    /// Numbers are decimal. Booleans are `1` and `0`. A vector (an array of numbers, such as
    /// `origin`) is its components joined by single spaces, `x y z`. Nulls, blobs and nested
    /// objects are left out.
    pub entities: Vec<BTreeMap<String, String>>,
}

/// Read the entities of one map from a game archive.
///
/// `citadel_dir` is the directory containing `pak01_dir.vpk`, i.e. `.../Deadlock/game/citadel`.
/// `vpk` is the archive relative to it (`maps/dl_midtown.vpk`) and `entry` the compiled entity
/// lump inside that archive (`maps/dl_midtown/entities/default_ents.vents_c`).
///
/// # Errors
///
/// If the archive cannot be opened, the entry is missing, the resource does not decode, or the
/// document has no `m_entityKeyValues` list.
pub fn entities(citadel_dir: impl AsRef<Path>, vpk: &str, entry: &str) -> Result<MapEntities> {
    let archive = citadel_dir.as_ref().join(vpk);
    let pack = Vpk::open(&archive).map_err(|e| vpk_error(e, &archive))?;
    let crc = pack
        .find(entry)
        .ok_or_else(|| Error::AssetNotFound(entry.to_owned()))?
        .crc;
    let doc = pack
        .read_resource_kv3(entry, BlockKind::DATA)
        .map_err(|e| match e {
            source2::Error::Vpk(e) => vpk_error(e, &archive),
            other => Error::Parse(format!("{entry}: {other}")),
        })?;
    Ok(MapEntities {
        crc,
        entities: flatten(&doc, entry)?,
    })
}

fn flatten(doc: &kv3::Document, entry: &str) -> Result<Vec<BTreeMap<String, String>>> {
    let list = doc
        .root
        .get("m_entityKeyValues")
        .and_then(kv3::Value::as_array)
        .ok_or_else(|| Error::Parse(format!("{entry}: no m_entityKeyValues list")))?;

    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let Some(values) = item
            .get("keyValues3Data")
            .and_then(|d| d.get("values"))
            .and_then(kv3::Value::as_object)
        else {
            continue;
        };
        out.push(
            values
                .iter()
                .filter_map(|(k, v)| scalar(v).map(|s| (k.to_owned(), s)))
                .collect(),
        );
    }
    Ok(out)
}

fn scalar(value: &kv3::Value) -> Option<String> {
    match value.kind() {
        kv3::Kind::String(s) => Some(s.clone()),
        kv3::Kind::Bool(b) => Some(if *b { "1" } else { "0" }.to_owned()),
        kv3::Kind::Int(n) => Some(n.to_string()),
        kv3::Kind::UInt(n) => Some(n.to_string()),
        kv3::Kind::Double(n) => Some(number(*n)),
        kv3::Kind::Array(items) => {
            let parts: Option<Vec<String>> = items.iter().map(scalar).collect();
            parts.map(|p| p.join(" "))
        }
        kv3::Kind::Null | kv3::Kind::Blob(_) | kv3::Kind::Object(_) => None,
    }
}

/// Floats are widened to `f64` on read, so a stored `0.1f32` would print as
/// `0.10000000149011612`. A value that survives a round trip through `f32` was stored at
/// that width and is printed at it.
fn number(n: f64) -> String {
    let narrow = n as f32;
    if f64::from(narrow) == n {
        narrow.to_string()
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUMP: &str = r#"<!-- kv3 encoding:text:version{e21c7f3c-8a33-41c5-9977-a76d3a32aa0d} format:generic:version{7412167c-06e9-4698-aff2-e63eb59037e7} -->
{
    m_entityKeyValues =
    [
        {
            keyValues3Data =
            {
                values =
                {
                    classname = "citadel_item_powerup_spawner"
                    origin = [ -3936.0, 136.0, 24.5 ]
                    teamnumber = 0
                    LaneNum = "4"
                    ratio = 0.1
                    enabled = true
                    skipped = null
                }
            }
            connections = [ { outputName = "OnStart" } ]
        },
        {
            keyValues3Data =
            {
                values =
                {
                    classname = "info_super_trooper_spawn"
                    BossName = "boss_rebel_t1_a"
                    origin = "10 -20 30"
                }
            }
        },
        { connections = [ ] },
    ]
}"#;

    fn flattened() -> Vec<BTreeMap<String, String>> {
        flatten(
            &kv3::Document::from_text(LUMP).expect("lump parses"),
            "test",
        )
        .expect("flatten")
    }

    #[test]
    fn keys_keep_their_case_and_entities_keep_their_order() {
        let ents = flattened();
        assert_eq!(ents.len(), 2, "the entry without values is dropped");
        assert_eq!(ents[0]["classname"], "citadel_item_powerup_spawner");
        assert_eq!(ents[0]["LaneNum"], "4");
        assert_eq!(ents[1]["BossName"], "boss_rebel_t1_a");
    }

    #[test]
    fn vectors_are_space_separated_and_strings_pass_through() {
        let ents = flattened();
        assert_eq!(ents[0]["origin"], "-3936 136 24.5");
        assert_eq!(ents[1]["origin"], "10 -20 30");
    }

    #[test]
    fn scalars_render_as_plain_strings() {
        let e = &flattened()[0];
        assert_eq!(e["teamnumber"], "0");
        assert_eq!(e["ratio"], "0.1");
        assert_eq!(e["enabled"], "1");
        assert!(!e.contains_key("skipped"));
    }

    #[test]
    fn a_document_without_an_entity_list_is_a_parse_error() {
        let doc =
            kv3::Document::from_text(&LUMP.replace("m_entityKeyValues", "other")).expect("parses");
        assert!(matches!(flatten(&doc, "test"), Err(Error::Parse(_))));
    }

    #[test]
    fn a_missing_archive_is_an_io_error() {
        let err = entities(std::env::temp_dir(), "maps/nope.vpk", "a.vents_c").unwrap_err();
        assert!(
            matches!(
                err,
                Error::Io {
                    kind: std::io::ErrorKind::NotFound,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    #[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
    fn the_midtown_lump_reads_from_the_install() {
        let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");
        let lump = entities(
            &dir,
            "maps/dl_midtown.vpk",
            "maps/dl_midtown/entities/default_ents.vents_c",
        )
        .expect("midtown entities");
        assert_eq!(lump.crc, 0x09ef_f23c);
        let spawner = lump
            .entities
            .iter()
            .find(|e| {
                e.get("classname").map(String::as_str) == Some("citadel_item_powerup_spawner")
                    && e.get("origin").is_some_and(|o| o.starts_with("-3936 136 "))
            })
            .expect("bridge buff spawner at (-3936, 136)");
        assert_eq!(
            spawner.get("classname").map(String::as_str),
            Some("citadel_item_powerup_spawner")
        );
    }
}
