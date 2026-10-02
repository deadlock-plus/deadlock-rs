//! Read-only probe against a running game: lists protobuf RTTI names, resolves vtables and
//! counts heap instances.

use deadlock_memory::attach_process;
use deadlock_walker::{PeImage, SearchConfig, VtableResolver, find_instances};

fn main() {
    let mem = attach_process("deadlock.exe").expect("attach");
    let modules = mem.modules().expect("modules");
    for m in &modules {
        if m.name.eq_ignore_ascii_case("client.dll") {
            println!("client.dll base={:#x} size={:#x}", m.base, m.size);
        }
    }
    let client = modules
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("client.dll"))
        .expect("client.dll");
    let image = PeImage::read(&*mem, client).expect("pe");
    for s in image.sections() {
        println!("section {s:?}");
    }
    let bytes = image.bytes();
    let needle = b".?AVCMsg";
    let mut names = std::collections::BTreeSet::new();
    let mut at = 0;
    while let Some(p) = memchr::memmem::find(&bytes[at..], needle) {
        let s = at + p;
        let end = bytes[s..].iter().position(|&b| b == 0).map_or(s, |e| s + e);
        names.insert(String::from_utf8_lossy(&bytes[s..end]).to_string());
        at = s + needle.len();
    }
    println!("{} CMsg type descriptors", names.len());
    for n in names
        .iter()
        .filter(|n| {
            ["Party", "Match", "Account", "Lobby", "PostGame"]
                .iter()
                .any(|k| n.contains(k))
        })
        .take(80)
    {
        println!("  {n}");
    }
    let resolver = VtableResolver::new(&image);
    let cfg = SearchConfig::default().excluding_module(client);
    for name in std::env::args().skip(1) {
        let vts = resolver.vtables(&name);
        println!("{name}: vtables {vts:x?}");
        for vt in vts {
            let hits = find_instances(&*mem, vt, &cfg).expect("search");
            println!(
                "  vt {vt:#x}: {} heap instances {:x?}",
                hits.len(),
                &hits[..hits.len().min(8)]
            );
            for &h in hits.iter().take(3) {
                if let Ok(b) = mem.read_bytes(h, 0x60) {
                    println!("  @{h:#x}");
                    for (i, c) in b.chunks(16).enumerate() {
                        println!("    +{:02x} {:02x?}", i * 16, c);
                    }
                }
            }
        }
    }
}
