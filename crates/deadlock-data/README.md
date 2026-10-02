# deadlock-data

Reference data for Deadlock: what the game *is*, rather than what it is currently
*doing*. Two catalogues, each mapping id -> internal class name -> display name:

* **`HeroCatalog`**: 57 heroes
* **`ItemCatalog`**: 726 entries (389 abilities, 251 shop upgrades, 86 weapons)

Deadlock puts abilities, upgrades and weapons in one id space, so a hero's ability and a
shop item resolve through the same lookup; `ItemKind` separates them. The ids match what
`deadlock-memory` reads out of `m_vecUpgrades` (purchases) and `m_vecAbilityUpgradeState`
(the AP spend order) exactly.

```rust
use deadlock_core::{ItemId, ItemNames};
use deadlock_data::ItemCatalog;

let items = ItemCatalog::bundled();
assert_eq!(items.item_name(ItemId(3977876567)), Some("Kinetic Dash"));
assert_eq!(items.item_name(ItemId(1011349580)), Some("Radiant Daggers"));
```

Three sources, layered, each behind its own feature:

| Source | Feature | Offline | Supplies |
|---|---|---|---|
| `HeroCatalog::bundled()` | `bundled` | yes | Vendored snapshot; never fails, no I/O |
| `HeroCatalog::from_game_dir()` | `client` | yes | The installed game's own localisation, 29 languages |
| `deadlock_data::api::fetch_heroes()` | `online` | no | deadlock-api.com |

`bundled` and `client` are on by default. At least one *roster* source - `bundled` or
`online` - has to be enabled, since names on their own are not addressable by id.
Dropping `bundled` takes about 205 KB of vendored JSON out of the build.

The client source is **two** features, because its halves cost very different things.
`client` reads the game's loose localisation files and needs nothing but `std`; `vpk`
reads the game's own id tables out of the compiled `vdata_c` files inside the VPK
archives, which needs a VPK reader and a zstd decoder. `vpk` is what lets the installed
game answer for the *roster* rather than for names alone - without it, `Source::Client`
supplies no ids, and a caller that expected a roster from an install gets none. It is off
by default so that a consumer wanting localised names does not compile an archive reader
it never calls.

One more, orthogonal to the sources: `serde` derives `Serialize`/`Deserialize` on the
public catalog types. It is not `dep:serde` - this crate parses its vendored JSON with
`serde_json` regardless, so the crate itself is mandatory here and only the derives are
gated.

A catalog is three tables that merge independently, because the sources do not carry the
same things: a **roster** (`Hero` / `Item`: id, class name, flags), **names**
(`class_name -> display name`, per language) and **art** (image URLs). Layering a source
that has only one of the three cannot blank the other two, and `provenance()` reports
which source answered for each. Art is a deadlock-api.com construct, so the installed
game never supplies it.

```rust
use deadlock_core::{HeroId, HeroNames};
use deadlock_data::HeroCatalog;

let mut heroes = HeroCatalog::bundled();
assert_eq!(heroes.hero_name(HeroId(1)), Some("Infernus"));

// Correct and localised, straight from the installed game.
let _ = heroes.merge_game_dir(".../Deadlock/game/citadel");
```

## Where the data actually lives

`class_name -> display name` is in **loose** localisation files under
`game/citadel/resource/localization/citadel_gc_hero_names/`, not inside a VPK, so no
unpacking is needed, and they are exactly matched to the installed build. 88 heroes.

`id -> class_name` is in `scripts/heroes.vdata` **inside** the VPK archives, which is why
this crate vendors a snapshot of it rather than parsing them. The API publishes both
together, but knows only 57 heroes, with 24 gaps in the id space, so an id the game uses
may have no API entry. Names known only by class name are kept in
`HeroCatalog::unlinked_names`.

`HeroCatalog::by_entity_class` recovers hero identity from a live entity class name
(`CCitadel_Ability_Bebop_LaserBeam` -> Bebop), which needs no id at all.

Item names are spread across several localisation bundles (upgrades in
`citadel_gc_mod_names`, abilities in `citadel_heroes`, plus a few in `citadel_main`,
`citadel_mods` and `citadel_attributes`), so the offline path reads all of them
(`ITEM_NAME_BUNDLES`). That covers ~509 of 726; the rest are unreleased or internal
entries the API leaves unlocalised too, and they fall back to their class name.

Licence: LGPL-3.0-or-later. See [LICENSE.md](https://github.com/deadlock-plus/deadlock-rs/blob/main/LICENSE.md).

The LGPL is a copyleft licence. Modifications to this library must be released
under the same terms; an application that merely uses it need not be.
