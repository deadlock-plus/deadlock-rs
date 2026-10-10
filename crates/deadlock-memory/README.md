# deadlock-memory

A read-only external process-memory client, in Rust. Attach to a process by image name,
read its memory, list its modules and mapped regions, and scan for byte signatures.

Nothing here is specific to any one program. The Deadlock reader built on top of it is
[`deadlock-reader`](https://github.com/deadlock-plus/deadlock-rs/tree/main/crates/deadlock-reader).

It is strictly read-only. There is no `WriteProcessMemory`, no injection, no hooking, no
remote thread creation, and no calling into the target anywhere in this crate. The only
Win32 calls it makes are `OpenProcess`, `ReadProcessMemory`, `VirtualQueryEx`, the
Toolhelp32 snapshot family and `CloseHandle`.

## Usage

```rust,no_run
use deadlock_memory::{attach_process, MemoryReader};

let process = attach_process("notepad.exe")?;
for module in process.modules()? {
    println!("{} @ {:#x} ({} bytes)", module.name, module.base, module.size);
}
# Ok::<(), deadlock_memory::Error>(())
```

`attach_process` picks the backend for the platform: `ReadProcessMemory` on Windows,
`process_vm_readv` on Linux, which also reads a Proton-hosted Windows process, and the Mach
task port (`mach_vm_read_overwrite`) on macOS, for a Windows build under Wine. The macOS
backend is best effort: it compiles, but has not been run against a real process, and
`task_for_pid` needs root.

On Linux, client-module checks use the full mapped filename, excluding Steam's own
client libraries when selecting the game process. Wine may map only a PE header from
its file and copy the sections into anonymous mappings. The backend reads the header's
`SizeOfImage` to include those sections, bounded by contiguous anonymous mappings.

On Linux, Yama's `ptrace_scope` decides whether another process may be read. A refused read
is reported as `Error::PtraceDenied` with advice matched to the current setting.

## Layout

| Module | Role |
|---|---|
| `mem` | `MemoryReader` trait: the whole OS surface, four required methods |
| `mock` | In-memory backend for tests, on any platform (`mock` feature) |
| `procmaps` | `/proc/<pid>/maps` parsing (pure, tested everywhere) |
| `attach` | Process choice and OS-error mapping, shared by the backends (pure, tested everywhere) |
| `linux` | Linux backend: `process_vm_readv` + `/proc/<pid>/maps` |
| `macos` | macOS backend: Mach task port + `proc_regionfilename` (best effort) |
| `process` | Windows backend: `ReadProcessMemory` + `VirtualQueryEx` |
| `region` | Committed RW region filter and region scanning |
| `sig` | AOB pattern engine + RIP-relative resolution (pure, no OS deps) |
| `error` | The crate's error type |

## Features

* `serde`: derive `Serialize` on `Module` and `Region`.
* `mock`: `mock::MockMemory`, a fake address space for testing without a running process.
  Off by default so it does not ship in release builds of consumers that never test with it.

## Licence

Licensed under either of the Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
or the MIT license ([`LICENSE-MIT`](LICENSE-MIT)), at your option.
