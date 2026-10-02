//! Live validation: derive layouts from the running client, walk every live
//! `CSOCitadelParty` and decode the bytes with the pinned `valveprotos` type.

use deadlock_walker::{
    ClientTables, PeImage, SearchConfig, VtableResolver, Walker, find_instances,
};
use prost::Message;
use valveprotos::deadlock::CsoCitadelParty;

fn main() {
    let mem = deadlock_memory::attach_process("deadlock.exe").expect("attach");
    let client = mem
        .modules()
        .expect("modules")
        .into_iter()
        .find(|m| m.name.eq_ignore_ascii_case("client.dll"))
        .expect("client.dll");
    let image = PeImage::read(&*mem, &client).expect("pe");
    let schema = deadlock_walker::client_schema(&image).expect("client schema");
    let tables = ClientTables::read(&image, &schema).expect("tables");
    let vt = VtableResolver::new(&image)
        .vtable("CSOCitadelParty")
        .expect("vtable");
    let cfg = SearchConfig::default().excluding_module(&client);
    let walker = Walker::new(&*mem, &schema, &tables);
    let meta = "CMsgMatchMetaDataContents";
    let meta_vt = VtableResolver::new(&image).vtable(meta).expect("vtable");
    for h in find_instances(&*mem, meta_vt, &cfg).expect("search") {
        match walker.serialize_checked(meta, h, meta_vt) {
            Ok(wire) => match valveprotos::deadlock::CMsgMatchMetaDataContents::decode(&wire[..]) {
                Ok(m) => {
                    let id = m.match_info.as_ref().and_then(|i| i.match_id);
                    println!(
                        "meta @{h:#x}: {} bytes, match_id {id:?}, players {}",
                        wire.len(),
                        m.match_info.as_ref().map_or(0, |i| i.players.len())
                    );
                    let mut m_core = m.clone();
                    if let Some(info) = m_core.match_info.as_mut() {
                        info.extra_messages.clear();
                    }
                    match saved_capture(id) {
                        Some(mut saved) => {
                            // The live client drops the bulky `match_paths` after loading.
                            if let Some(info) = saved.match_info.as_mut() {
                                info.match_paths = None;
                            }
                            println!(
                                "  equals the cached capture (minus match_paths): {}",
                                saved == m
                            );
                            if let Some(info) = saved.match_info.as_mut() {
                                info.extra_messages.clear();
                            }
                            println!(
                                "  equals it also ignoring extra_messages: {}",
                                saved == m_core
                            );
                            if saved != m && std::env::var("DIFF").is_ok() {
                                let (a, b) = (format!("{saved:#?}"), format!("{m:#?}"));
                                println!(
                                    "  cached {} bytes vs live {} bytes",
                                    saved.encode_to_vec().len(),
                                    m.encode_to_vec().len()
                                );
                                for (n, (x, y)) in a.lines().zip(b.lines()).enumerate() {
                                    if x != y {
                                        println!(
                                            "  first debug diff at line {n}:
    cached: {x}
    live:   {y}"
                                        );
                                        for l in b.lines().skip(n + 1).take(14) {
                                            println!("    live+   {l}");
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                        None => println!("  no cached capture"),
                    }
                }
                Err(e) => println!("meta @{h:#x}: decode failed: {e}"),
            },
            Err(e) => println!("meta @{h:#x}: walk failed: {e}"),
        }
    }
    for h in find_instances(&*mem, vt, &cfg).expect("search") {
        match walker.serialize_checked("CSOCitadelParty", h, vt) {
            Ok(wire) => match CsoCitadelParty::decode(&wire[..]) {
                Ok(p) => {
                    println!(
                        "@{h:#x}: {} wire bytes, re-encodes identically: {}",
                        wire.len(),
                        p.encode_to_vec() == wire
                    );
                    println!("{p:#?}");
                }
                Err(e) => println!("@{h:#x}: decode failed: {e}"),
            },
            Err(e) => println!("@{h:#x}: walk failed: {e}"),
        }
    }
}

/// The Statlocker cache copy of the match, if it exists on this machine.
fn saved_capture(id: Option<u64>) -> Option<valveprotos::deadlock::CMsgMatchMetaDataContents> {
    use std::io::Read;
    let path = format!(
        r"C:\Users\user\AppData\Local\statlocker-companion\metadata_cache\match_{}.pb.gz",
        id?
    );
    let gz = std::fs::read(path).ok()?;
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_end(&mut raw)
        .ok()?;
    valveprotos::deadlock::CMsgMatchMetaDataContents::decode(&raw[..]).ok()
}
