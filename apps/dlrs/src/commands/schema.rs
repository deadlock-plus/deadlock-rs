//! Schema inspection: `dlrs schema`, `enums`, `probe-schema`, `fields`.

use deadlock_reader::fields as tbl;
use deadlock_reader::schema::SchemaLayout;

use super::attach;

pub fn schema(filter: Option<&str>) -> i32 {
    let Some(r) = attach() else { return 1 };
    let Some(s) = r.schema() else {
        eprintln!(
            "schema unresolved: {}",
            r.schema_error()
                .map(std::string::ToString::to_string)
                .unwrap_or_default()
        );
        eprintln!("try `dlrs probe-schema`");
        return 1;
    };
    match filter {
        Some(f) => {
            let mut shown = 0usize;
            for name in s.find_classes(f) {
                shown += 1;
                let c = &s.classes[name];
                outln!("{name}  (size {}, {} fields)", c.size, c.fields.len());
                let mut fs: Vec<_> = c.fields.iter().collect();
                fs.sort_by_key(|(_, off)| **off);
                for (fname, off) in fs {
                    outln!("    +{off:#06x}  {fname}");
                }
            }
            outln!("{}", shown_of(shown, s.class_count(), f));
        }
        None => {
            let mut names: Vec<&String> = s.classes.keys().collect();
            names.sort();
            for n in names {
                outln!("{n}");
            }
            outln!("\n{} classes", s.class_count());
        }
    }
    0
}

pub fn enums(filter: Option<&str>) -> i32 {
    let Some(r) = attach() else { return 1 };
    let Some(s) = r.schema() else {
        eprintln!("schema unresolved; enum values come from the runtime schema only");
        return 1;
    };
    let mut names: Vec<&String> = s
        .enums
        .keys()
        .filter(|n| filter.map(|f| n.contains(f)).unwrap_or(true))
        .collect();
    names.sort();
    for n in &names {
        let e = &s.enums[*n];
        outln!("{} ({} bytes, {} values)", e.name, e.size, e.values.len());
        let mut vals: Vec<(&i64, &String)> = e.values.iter().collect();
        vals.sort();
        for (v, nm) in vals {
            outln!("  {v:>5}  {nm}");
        }
    }
    outln!("\n{} enums shown of {}", names.len(), s.enums.len());
    0
}

pub fn probe_schema() -> i32 {
    let Some(r) = attach() else { return 1 };
    outln!("probing CSchemaSystem::m_TypeScopes offset...\n");

    let mut found = false;
    for &off in SchemaLayout::TYPE_SCOPE_CANDIDATES {
        let layout = SchemaLayout {
            system_type_scopes: off,
            ..SchemaLayout::DEADLOCK
        };
        match deadlock_reader::schema::SchemaIndex::walk(
            r.memory(),
            r.globals().schema_system,
            layout,
        ) {
            Ok(idx) => {
                outln!(
                    "  {off:#06x}  OK   {} classes, scopes: {}",
                    idx.class_count(),
                    idx.scopes.join(", ")
                );
                // A correct layout must be able to see classes we know exist.
                let sanity = ["C_CitadelPlayerPawn", "CCitadelPlayerController"];
                let hits = sanity
                    .iter()
                    .filter(|c| idx.classes.contains_key(**c))
                    .count();
                outln!("        sanity classes present: {hits}/{}", sanity.len());
                found = true;
            }
            Err(e) => outln!("  {off:#06x}  --   {e}"),
        }
    }
    if !found {
        outln!(
            "\nNo candidate offset produced a plausible walk. The rest of \
             SchemaLayout (CUtlTSHash / class binding offsets) likely needs \
             correcting too - see src/schema.rs."
        );
        return 1;
    }
    0
}

pub fn fields() -> i32 {
    outln!(
        "{:<38} {:<30} {:>10}  {:<}",
        "class",
        "field",
        "fallback",
        "inferred"
    );
    for f in tbl::WIN64_FIELDS {
        outln!(
            "{:<38} {:<30} {:>10}  {}",
            f.class,
            f.field,
            f.fallback
                .map(|v| format!("{v:#x}"))
                .unwrap_or_else(|| "-".into()),
            if f.inferred { "yes" } else { "" }
        );
    }
    outln!("\n{} pairs", tbl::WIN64_FIELDS.len());
    0
}

/// The trailing count line for a filtered listing.
///
/// Every other listing command ends with one - `dlrs enums` says "N enums shown of M",
/// `dlrs entities` says "N shown of M live entities" - and `dlrs schema <filter>` said
/// nothing at all. A filter that matched nothing therefore produced **no output and exit
/// 0**, which is indistinguishable from a command that ran and did nothing, and is the
/// same silence-for-absence this project refuses elsewhere.
fn shown_of(shown: usize, total: usize, filter: &str) -> String {
    format!("\n{shown} classes shown of {total} matching {filter:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filter that matches nothing still says so.
    ///
    /// The zero case is the whole reason this line exists; the non-zero case is here so a
    /// change that only special-cases zero cannot pass.
    #[test]
    fn a_filtered_listing_always_reports_how_many_it_showed() {
        assert_eq!(
            shown_of(0, 3606, "NoSuchClass"),
            "\n0 classes shown of 3606 matching \"NoSuchClass\""
        );
        assert_eq!(
            shown_of(2, 3606, "Citadel"),
            "\n2 classes shown of 3606 matching \"Citadel\""
        );
    }
}
