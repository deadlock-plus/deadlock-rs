//! The transcribed tunables still equal the numbers in the game's archive.
//!
//! `deadlock-reader` transcribes a handful of constants out of the shipped `vdata_c` files
//! — rejuvenator durations, the urn drop, the rift radius, objective sight ranges, the
//! Midboss spawn schedule. Nothing else ties those constants to the archive: editing
//! `DEFAULT_MIDBOSS_SPAWN_INTERVAL` from `420` to `999` passes the rest of the workspace
//! suite, live tests included.
//!
//! `dlrs` depends on `deadlock-reader` and is not published, so a dev-dependency on
//! `source2` breaks no layering rule and puts both sides in view at once. A library crate
//! could not: no crate in `crates/` depends on a sibling.

use deadlock_reader::tunables as t;
use source2::ext::VpkResourceKv3;
use source2::kv3;
use source2::resource::BlockKind;
use source2::vpk::Vpk;

/// Read one `vdata_c` document out of the archive.
fn doc(dir: &str, path: &str) -> kv3::Document {
    let vpk = Vpk::open(std::path::Path::new(dir).join("pak01_dir.vpk")).expect("open vpk");
    vpk.read_resource_kv3(path, BlockKind::DATA).expect("kv3")
}

/// A nested `f64`, by path of object keys.
fn at(d: &kv3::Document, keys: &[&str]) -> Option<f64> {
    let mut node = d.root.as_object()?;
    let (last, rest) = keys.split_last()?;
    for k in rest {
        node = node.get(k)?.as_object()?;
    }
    node.get(last)?.as_f64()
}

#[test]
#[ignore = "needs an installed game; set DEADLOCK_CITADEL_DIR"]
fn every_transcribed_tunable_still_matches_the_archive() {
    let dir = std::env::var("DEADLOCK_CITADEL_DIR").expect("set DEADLOCK_CITADEL_DIR");

    let generic = doc(&dir, "scripts/generic_data.vdata_c");
    let misc = doc(&dir, "scripts/misc.vdata_c");
    let npc = doc(&dir, "scripts/npc_units.vdata_c");

    let checks: [(&str, f32, Option<f64>); 8] = [
        (
            "DEFAULT_REJUV_BUFF_DURATION",
            t::DEFAULT_REJUV_BUFF_DURATION,
            at(&generic, &["m_RejuvParams", "m_flRejuvinatorBuffDuration"]),
        ),
        (
            "DEFAULT_REJUV_EXPIRATION_WARNING",
            t::DEFAULT_REJUV_EXPIRATION_WARNING,
            at(
                &generic,
                &["m_RejuvParams", "m_flRejuvinatorExpirationWarningTiming"],
            ),
        ),
        (
            "DEFAULT_REJUV_DROP_DURATION",
            t::DEFAULT_REJUV_DROP_DURATION,
            at(&generic, &["m_RejuvParams", "m_flRejuvinatorDropDuration"]),
        ),
        (
            "DEFAULT_URN_DROP_DURATION",
            t::DEFAULT_URN_DROP_DURATION,
            at(&generic, &["m_IdolParams", "m_flIdolDropDuration"]),
        ),
        (
            "DEFAULT_MIDBOSS_SIGHT_RADIUS",
            t::DEFAULT_MIDBOSS_SIGHT_RADIUS,
            at(&npc, &["npc_super_neutral", "m_flSightRangePlayers"]),
        ),
        (
            "DEFAULT_WALKER_SIGHT_RADIUS",
            t::DEFAULT_WALKER_SIGHT_RADIUS,
            at(&npc, &["npc_boss_tier2", "m_flSightRangePlayers"]),
        ),
        (
            "DEFAULT_MIDBOSS_SPAWN_INTERVAL",
            t::DEFAULT_MIDBOSS_SPAWN_INTERVAL,
            at(
                &misc,
                &["neutral_camp_midboss", "m_iSpawnIntervalInSeconds"],
            ),
        ),
        (
            "DEFAULT_MIDBOSS_INTERVAL_MIN",
            t::DEFAULT_MIDBOSS_INTERVAL_MIN,
            at(&misc, &["neutral_camp_midboss", "m_iSpawnIntervalMin"]),
        ),
    ];

    let mut wrong = Vec::new();
    for (name, constant, shipped) in checks {
        match shipped {
            Some(v) if (v - f64::from(constant)).abs() < 1e-6 => {}
            other => wrong.push(format!("{name} is {constant}, archive says {other:?}")),
        }
    }
    assert!(
        wrong.is_empty(),
        "transcribed tunables no longer match the archive: {wrong:#?}"
    );

    let koth = at(&generic, &["m_KothParams", "m_flKothRadius"]).expect("m_flKothRadius");
    let expect = koth as f32 * t::UNITS_PER_METER;
    assert!(
        (t::DEFAULT_RIFT_RADIUS - expect).abs() < 0.01,
        "DEFAULT_RIFT_RADIUS is {}, but {koth} m at {} u/m is {expect}",
        t::DEFAULT_RIFT_RADIUS,
        t::UNITS_PER_METER
    );
}
