//! Live check of the match metadata objects the client holds: lists the resident matches
//! and compares each with the Statlocker cache copy when one exists.
//!
//! `cargo run --release -p deadlock-walker --example metadata [match_id ...]`

use std::io::Read;
use std::time::Instant;

use deadlock_walker::GcSession;
use prost::Message;
use valveprotos::deadlock::CMsgMatchMetaDataContents as Meta;

fn main() {
    let mem = deadlock_memory::attach_process("deadlock.exe").expect("attach");
    let account = deadlock_reader::steam::active_account_id()
        .expect("account id")
        .expect("signed in");
    let mut gc = GcSession::new(&*mem, account).expect("session");

    let t = Instant::now();
    let all = gc.match_metadata_all(&*mem).expect("metadata");
    println!("{} resident in {:.2?}", all.len(), t.elapsed());

    let mut ids: Vec<u64> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    if ids.is_empty() {
        ids = all
            .iter()
            .filter_map(|m| m.match_info.as_ref()?.match_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
    }

    for id in ids {
        let t = Instant::now();
        let Some(live) = gc.match_metadata(&*mem, id).expect("metadata") else {
            println!("match {id}: not resident");
            continue;
        };
        let players = live.match_info.as_ref().map_or(0, |i| i.players.len());
        println!(
            "match {id}: {players} players, {} bytes, read in {:.2?}",
            live.encoded_len(),
            t.elapsed()
        );
        match saved_capture(id) {
            Some(saved) => println!(
                "  equals the Statlocker cache ignoring match_paths/extra_messages: {}",
                trimmed(saved) == trimmed(live)
            ),
            None => println!("  no Statlocker cache file"),
        }
    }
}

fn trimmed(mut m: Meta) -> Meta {
    if let Some(info) = m.match_info.as_mut() {
        info.match_paths = None;
        info.extra_messages.clear();
    }
    m
}

fn saved_capture(id: u64) -> Option<Meta> {
    let path = format!(
        r"C:\Users\user\AppData\Local\statlocker-companion\metadata_cache\match_{id}.pb.gz"
    );
    let gz = std::fs::read(path).ok()?;
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_end(&mut raw)
        .ok()?;
    Meta::decode(&raw[..]).ok()
}
