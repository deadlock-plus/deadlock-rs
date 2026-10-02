//! `dlrs entities`: list live entities.

use deadlock_reader::entity::EntitySnapshot;

use super::attach;

pub fn entities(filter: Option<&str>) -> i32 {
    let Some(r) = attach() else { return 1 };
    let snap = match r.entities() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("entity walk failed: {e}");
            return 1;
        }
    };
    print_entities(&snap, filter);
    0
}

fn print_entities(snap: &EntitySnapshot, filter: Option<&str>) {
    outln!(
        "{:>6}  {:<18}  {:<40}  {:<}",
        "idx",
        "instance",
        "class",
        "designer"
    );
    let mut shown = 0;
    for e in snap.all() {
        if let Some(f) = filter
            && !e.best_name().contains(f)
        {
            continue;
        }
        outln!(
            "{:>6}  {:#018x}  {:<40}  {}",
            e.index,
            e.instance,
            e.class_name,
            e.designer_name
        );
        shown += 1;
    }
    outln!("\n{shown} shown of {} live entities", snap.len());
}
