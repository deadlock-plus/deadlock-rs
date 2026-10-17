//! Check of `PartyQueue`: print the party search start, beside the solo flag, as they change
//! in a running client.
//!
//! `cargo run --release -p deadlock-walker --example party_queue`
//!
//! Given the path of a `client.dll` on disk, only locate the slot in that file and print
//! its RVA and the start time offset; no game needed.

use deadlock_walker::{PartyQueue, PeImage};

fn main() {
    match std::env::args_os().nth(1) {
        Some(path) => offline(std::path::Path::new(&path)),
        None => live::main(),
    }
}

fn offline(path: &std::path::Path) {
    let file = std::fs::read(path).expect("read client.dll");
    let image = PeImage::from_bytes(0, map(&file)).expect("pe");
    let location = PartyQueue::locate(&image).expect("locate");
    println!("party slot RVA {:#x}", location.slot);
    println!("start time offset {:#x}", location.start_offset);
}

/// Lay a PE file out as the loader would, so RVAs index it.
fn map(file: &[u8]) -> Vec<u8> {
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([file[at], file[at + 1]]));
    let u32_at = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap()) as usize;
    let nt = u32_at(0x3c);
    let opt = nt + 24;
    let image_size = u32_at(opt + 56);
    let headers = u32_at(opt + 60);
    let mut mapped = vec![0; image_size];
    mapped[..headers].copy_from_slice(&file[..headers]);
    let table = opt + u16_at(nt + 20);
    for i in 0..u16_at(nt + 6) {
        let at = table + i * 40;
        let (rva, raw_size, raw_at) = (u32_at(at + 12), u32_at(at + 16), u32_at(at + 20));
        let len = raw_size.min(image_size - rva).min(file.len() - raw_at);
        mapped[rva..rva + len].copy_from_slice(&file[raw_at..raw_at + len]);
    }
    mapped
}

#[cfg(any(windows, target_os = "linux"))]
mod live {
    use std::time::Duration;

    use deadlock_walker::{PartyQueue, PeImage, QueueFlag};

    pub fn main() {
        let mem = deadlock_memory::attach_process("deadlock.exe").expect("attach");
        let client = mem.module("client.dll").expect("client.dll");
        let image = PeImage::read(&*mem, &client).expect("pe");
        let mut flag = QueueFlag::from_image(&*mem, &image).expect("queue flag");
        let queue = PartyQueue::from_image(&*mem, &image).expect("locate");
        drop(image);
        let location = queue.location();
        println!(
            "party slot {:#x}, start time offset {:#x}",
            location.slot, location.start_offset
        );
        let mut last = None;
        loop {
            let state = (
                flag.queueing(&*mem).expect("flag"),
                queue.read(&*mem).expect("read"),
            );
            if Some(state) != last {
                println!("solo flag {:?}, party {:?}", state.0, state.1);
                last = Some(state);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod live {
    pub fn main() {
        eprintln!("reading a live process is supported on Windows and Linux only");
        std::process::exit(1);
    }
}
