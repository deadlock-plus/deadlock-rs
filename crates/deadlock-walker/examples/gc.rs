//! Live check of `GcSession`: build it, sweep, read every kind and time the steps.
//!
//! `cargo run --release -p deadlock-walker --example gc`

#[cfg(any(windows, target_os = "linux"))]
mod live {
    use std::time::Instant;

    use deadlock_walker::GcSession;

    pub fn main() {
        let mem = deadlock_memory::attach_process("deadlock.exe").expect("attach");
        let account = deadlock_reader::steam::active_account_id()
            .expect("account id")
            .expect("signed in");

        let t = Instant::now();
        let mut gc = GcSession::new(&*mem, account).expect("session");
        println!("session built in {:.2?}", t.elapsed());

        let t = Instant::now();
        let report = gc.sweep(&*mem).expect("sweep");
        println!("sweep {report:?} in {:.2?}", t.elapsed());

        let t = Instant::now();
        let parties = gc.parties(&*mem).expect("parties");
        println!("{} parties in {:.2?}", parties.len(), t.elapsed());
        for p in &parties {
            println!(
                "  party {:?} members {:?}",
                p.party_id,
                p.members.iter().map(|m| m.account_id).collect::<Vec<_>>()
            );
        }
        println!("lobby {:?}", gc.lobby(&*mem).map(|l| l.map(|l| l.lobby_id)));
        println!(
            "game account {:?}",
            gc.game_account(&*mem)
                .map(|a| a.map(|a| (a.wins, a.losses)))
        );
        println!(
            "stats {:?}",
            gc.account_stats(&*mem).map(|s| s.map(|s| s.stats.len()))
        );
        println!("heroes {:?}", gc.account_heroes(&*mem).map(|h| h.len()));
        println!(
            "hideout {:?}",
            gc.hideout(&*mem).map(|h| h.map(|h| h.hideout_lobby_id))
        );
        let builds = gc.hero_builds(&*mem).expect("builds");
        let mut distinct: Vec<_> = builds.iter().map(|b| b.hero_build_id).collect();
        distinct.sort_unstable();
        distinct.dedup();
        println!("builds {} ({} distinct ids)", builds.len(), distinct.len());
        if std::env::var("DETAIL").is_ok() {
            for b in &builds {
                println!(
                    "  id {:?} v{:?} hero {:?} {:?} updated {:?} items {}",
                    b.hero_build_id,
                    b.version,
                    b.hero_id,
                    b.name,
                    b.last_updated_timestamp,
                    b.details.as_ref().map_or(0, |d| d.mod_categories.len())
                );
            }
            if let Ok(Some(s)) = gc.account_stats(&*mem) {
                println!(
                    "stats account {:?}: {} entries",
                    s.account_id,
                    s.stats.len()
                );
                for h in s.stats.iter().take(8) {
                    println!("  hero {:?} stat_ids {}", h.hero_id, h.stat_id.len());
                }
            }
        }
        println!(
            "post game {:?}",
            gc.post_game_progress(&*mem).map(|p| p.map(|p| p.match_id))
        );

        let t = Instant::now();
        for _ in 0..1000 {
            let _ = gc.party(&*mem);
        }
        println!("pinned party re-read {:.1?} each", t.elapsed() / 1000);
        println!("stats {:?}", gc.stats());
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn main() {
    live::main();
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    eprintln!("reading a live process is supported on Windows and Linux only");
    std::process::exit(1);
}
