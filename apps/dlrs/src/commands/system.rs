//! Process and account plumbing: `dlrs regions` and `dlrs steam`.

use deadlock_reader::steam as st;

use super::attach;

pub fn regions() -> i32 {
    let Some(r) = attach() else { return 1 };
    match r.memory().regions() {
        Ok(rs) => {
            let total: usize = rs.iter().map(|x| x.size).sum();
            for x in rs.iter().take(40) {
                outln!("{:#018x}  {:>12}", x.base, x.size);
            }
            outln!(
                "\n{} regions, {:.1} MiB scannable",
                rs.len(),
                total as f64 / (1024.0 * 1024.0)
            );
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

pub fn steam() -> i32 {
    match st::active_account_id() {
        Ok(Some(id)) => {
            outln!("account_id  {id}");
            outln!("steam64     {}", st::to_steam64(id));
            0
        }
        Ok(None) => {
            outln!("no Steam user is currently signed in");
            0
        }
        Err(e) => {
            eprintln!("could not read local account_id from registry: {e}");
            1
        }
    }
}
