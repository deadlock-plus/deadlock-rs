//! Live check: derive layouts from the running client and dump one message class.

#[cfg(any(windows, target_os = "linux"))]
mod live {
    use deadlock_memory::attach_process;
    use deadlock_walker::{
        ClientTables, LayoutSource, PeImage, SearchConfig, VtableResolver, Walker, find_instances,
    };

    pub fn main() {
        let mem = attach_process("deadlock.exe").expect("attach");
        let modules = mem.modules().expect("modules");
        let client = modules
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case("client.dll"))
            .expect("client.dll");
        let image = PeImage::read(&*mem, client).expect("pe");
        let schema = deadlock_walker::client_schema(&image).expect("client schema");
        let t0 = std::time::Instant::now();
        let tables = ClientTables::read(&image, &schema).expect("tables");
        println!(
            "{} layouts, {} skipped, in {:?}",
            tables.len(),
            tables.skipped().len(),
            t0.elapsed()
        );
        for s in tables.skipped().iter().take(15) {
            println!("  skipped {}: {}", s.message, s.reason);
        }
        let resolver = VtableResolver::new(&image);
        let cfg = SearchConfig::default().excluding_module(client);
        for name in std::env::args().skip(1) {
            if let Some(rest) = name.strip_prefix("raw:") {
                let (a, l) = rest.split_once(':').unwrap();
                let a = u64::from_str_radix(a.trim_start_matches("0x"), 16).unwrap();
                let l: usize = l.parse().unwrap();
                let raw = mem.read_bytes(a, l).expect("raw");
                for (i, c) in raw.chunks(16).enumerate() {
                    println!("raw {:#x} {:02x?}", a + i as u64 * 16, c);
                }
                continue;
            }
            let Some(msg) = schema.message(&name) else {
                for n in schema.names().filter(|n| n.contains(&name)) {
                    println!("candidate {n}");
                }
                continue;
            };
            let Some(layout) = tables.layout(&name) else {
                println!(
                    "{name}: no layout; skipped: {:?}",
                    tables.skipped().iter().find(|s| s.message == name)
                );
                let vt = resolver.vtable(&name).expect("vt");
                for &h in find_instances(&*mem, vt, &cfg)
                    .expect("search")
                    .iter()
                    .take(3)
                {
                    let raw = mem.read_bytes(h, 0x170).expect("read");
                    println!("@{h:#x}");
                    for (i, c) in raw.chunks(16).enumerate() {
                        println!("  +{:03x} {:02x?}", i * 16, c);
                    }
                }
                for f in &msg.fields {
                    println!(
                        "  #{:<3} {:<28} {:?}{}",
                        f.number,
                        f.name,
                        f.kind,
                        if f.repeated { " repeated" } else { "" }
                    );
                }
                continue;
            };
            println!(
                "{name}: size {:#x} has_bits@{:#x}",
                layout.size, layout.has_bits_offset
            );
            for f in &msg.fields {
                println!(
                    "  #{:<3} {:<28} {:?}{} -> {:?}",
                    f.number,
                    f.name,
                    f.kind,
                    if f.repeated { " repeated" } else { "" },
                    layout.fields.get(&f.number)
                );
            }
            let vt = resolver.vtable(&name).expect("vt");
            let hits = find_instances(&*mem, vt, &cfg).expect("search");
            println!("{} heap instances", hits.len());
            for &h in hits.iter().take(4) {
                let raw = mem.read_bytes(h, layout.size).expect("read");
                println!("@{h:#x}");
                for (i, c) in raw.chunks(16).enumerate() {
                    println!("  +{:03x} {:02x?}", i * 16, c);
                }
                match Walker::new(&*mem, &schema, &tables).serialize_checked(&name, h, vt) {
                    Ok(w) => println!("  wire {} bytes: {}", w.len(), hex(&w)),
                    Err(e) => println!("  walk error: {e}"),
                }
            }
        }
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
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
