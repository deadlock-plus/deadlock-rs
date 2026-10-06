//! Live diagnostic: per kind, how many heap objects carry the vtable and how many walk.
//!
//! A kind with no hits while the game is in the matching state means the heap search is not
//! reaching the object (see the README's live checks).
//!
//! `cargo run --release -p deadlock-walker --example kinds`

#[cfg(any(windows, target_os = "linux"))]
mod live {
    use deadlock_walker::{
        ClientTables, Kind, PeImage, SearchConfig, VtableResolver, Walker, find_instances,
    };

    pub fn main() {
        let mem = deadlock_memory::attach_process("deadlock.exe").expect("attach");
        let client = mem.module("client.dll").expect("client.dll");
        let image = PeImage::read(&*mem, &client).expect("pe");
        let schema = deadlock_walker::client_schema(&image).expect("client schema");
        let tables = ClientTables::read(&image, &schema).expect("tables");
        let resolver = VtableResolver::new(&image);
        let cfg = SearchConfig::default().excluding_module(&client);
        let walker = Walker::new(&*mem, &schema, &tables);
        for kind in Kind::ALL {
            let name = kind.message();
            let Some(&vt) = resolver.vtables(name).first() else {
                println!("{name}: no vtable");
                continue;
            };
            let hits = find_instances(&*mem, vt, &cfg).expect("search");
            let walked = hits
                .iter()
                .filter(|&&h| walker.serialize_checked(name, h, vt).is_ok())
                .count();
            println!("{name}: {} hits, {walked} walk", hits.len());
        }
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
