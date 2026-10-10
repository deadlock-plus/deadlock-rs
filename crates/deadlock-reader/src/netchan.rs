use deadlock_memory::mem::MemoryReader;
use deadlock_memory::sig::{Pattern, SigDesc, scan_and_resolve};

/// Module holding the engine global that leads to the net channel.
pub const ENGINE_MODULE: &str = "engine2.dll";

/// Module holding the `CNetChan` accessors the field offsets are read from.
pub const NETWORKSYSTEM_MODULE: &str = "networksystem.dll";

/// The engine global whose first slot is the client's `CNetChan*`.
///
/// ```text
/// mov    r8, [rip+G]          ; <- disp32 at +3, ends at +7
/// test   r8, r8
/// jz     ...
/// movsxd rcx, edx
/// lea    rax, [rcx+rcx*2]
/// mov    rax, [r8+rax*8+0xF0]
/// ```
pub const ENGINE_GLOBAL: SigDesc = SigDesc {
    name: "net_chan_global",
    pattern: "4C 8B 05 ?? ?? ?? ?? 4D 85 C0 74 10 48 63 CA 48 8D 04 49 49 8B 84 C0 F0 00 00 00 C3 33 C0 C3",
    disp_off: 3,
    instr_len: 7,
};

/// Byte offset of the channel pointer inside the object `G` points to.
const CHAN_SLOT: u64 = 0xF0;

/// Start of the ping accessor; the first disp32 is an unrelated field. The ping load is
/// found by [`PING_FIELD`] further into the same function.
const PING_GETTER: &str = "40 57 48 83 EC 30 8B 91 ?? ?? ?? ?? 48 8B F9 85 D2 0F 84";

/// `mov eax, [rdi+PING]; test eax, eax; js` inside the ping accessor, disp32 at +2.
const PING_FIELD: &str = "8B 87 ?? ?? ?? ?? 85 C0 78";

/// How far into the ping accessor [`PING_FIELD`] is searched for.
const PING_FIELD_WINDOW: usize = 0x100;

/// One flow-indexed `f32` accessor: `cmp edx,1; mov eax,IN; mov r8d,OUT; cmovne eax,r8d;
/// movss xmm0,[rax+rcx]; ret`. The imm32 at +4 is the inbound field, the one at +10 the
/// outbound field.
///
/// Exactly three accessors have this shape, adjacent and in this order: loss, late, jitter.
const FLOW_GETTER: &str = "83 FA 01 B8 ?? ?? ?? ?? 41 B8 ?? ?? ?? ?? 41 0F 45 C0 F3 0F 10 04 08 C3";
const FLOW_GETTER_COUNT: usize = 3;

/// Offsets from here up are not a field of a `CNetChan`.
const MAX_FIELD_OFFSET: u32 = 0x1_0000;

const MAX_PING_MS: i32 = 10_000;
const MAX_JITTER_MS: f32 = 10_000.0;

/// Why [`NetChanAnchors::read`] or [`NetChanAnchors::resolve`] produced nothing.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum NetChanError {
    /// There is no net channel right now: the game is not connected to a server.
    Unavailable,
    /// A module is not loaded, or a signature missed or yielded an impossible offset.
    Unresolved(deadlock_memory::Error),
    /// A field was read, but holds a value it cannot.
    Implausible {
        /// Field name.
        field: &'static str,
        /// The decoded value.
        value: f64,
    },
    /// A read from the process failed.
    Memory(deadlock_memory::Error),
}

impl std::fmt::Display for NetChanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetChanError::Unavailable => write!(f, "no net channel: not connected to a server"),
            NetChanError::Unresolved(e) => write!(f, "net channel unresolved: {e}"),
            NetChanError::Implausible { field, value } => {
                write!(
                    f,
                    "net channel {field} reads {value}, which is out of range"
                )
            }
            NetChanError::Memory(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for NetChanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NetChanError::Unresolved(e) | NetChanError::Memory(e) => Some(e),
            _ => None,
        }
    }
}

/// The user's own connection quality to the game server.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NetChanStats {
    /// Round-trip time in milliseconds.
    pub ping_ms: i32,
    /// Fraction of server-to-client packets lost, 0..=1.
    pub loss_down: f32,
    /// Fraction of client-to-server packets lost, 0..=1.
    pub loss_up: f32,
    /// Inbound jitter in milliseconds.
    pub jitter_in_ms: f32,
    /// Outbound jitter in milliseconds.
    pub jitter_out_ms: f32,
}

/// Field offsets inside `CNetChan`, derived from the engine's own accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetChanOffsets {
    /// Ping, `i32` milliseconds.
    pub ping: u32,
    /// Inbound loss, `f32`.
    pub loss_down: u32,
    /// Outbound loss, `f32`.
    pub loss_up: u32,
    /// Inbound jitter, `f32` milliseconds.
    pub jitter_in: u32,
    /// Outbound jitter, `f32` milliseconds.
    pub jitter_out: u32,
}

/// Where the net channel lives and how it is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetChanAnchors {
    global: u64,
    offsets: NetChanOffsets,
}

impl NetChanAnchors {
    /// Resolve against the loaded `engine2.dll` and `networksystem.dll`.
    ///
    /// Copies both images and scans each once; callers should keep the result.
    ///
    /// # Errors
    ///
    /// [`NetChanError::Unresolved`] if a module is not loaded or a signature misses.
    pub fn resolve(mem: &dyn MemoryReader) -> Result<Self, NetChanError> {
        let image = |name: &str| -> Result<(u64, Vec<u8>), NetChanError> {
            let module = mem.module(name).map_err(NetChanError::Unresolved)?;
            let (bytes, read) = mem.read_image(module.base, module.size);
            if read == 0 {
                return Err(NetChanError::Unresolved(
                    deadlock_memory::Error::ProcessGone {
                        pid: mem.pid(),
                        os: 0,
                    },
                ));
            }
            Ok((module.base, bytes))
        };
        let (engine_base, engine) = image(ENGINE_MODULE)?;
        let (_, networksystem) = image(NETWORKSYSTEM_MODULE)?;
        Self::from_images(&engine, engine_base, &networksystem)
    }

    /// Resolve from module image copies; `engine` must start at `engine_base`.
    ///
    /// # Errors
    ///
    /// [`NetChanError::Unresolved`] if a signature misses or yields an offset no
    /// `CNetChan` field can have.
    pub fn from_images(
        engine: &[u8],
        engine_base: u64,
        networksystem: &[u8],
    ) -> Result<Self, NetChanError> {
        let global = scan_and_resolve(&ENGINE_GLOBAL, engine, engine_base, engine.len())
            .map_err(NetChanError::Unresolved)?;

        let ping_getter = find("net_ping_getter", PING_GETTER, networksystem)?;
        let window = &networksystem[ping_getter..];
        let window = &window[..window.len().min(PING_FIELD_WINDOW)];
        let ping = find("net_ping_field", PING_FIELD, window)?;
        let flow = Pattern::parse(FLOW_GETTER).map_err(NetChanError::Unresolved)?;
        let flow = flow.find_all(networksystem);
        if flow.len() != FLOW_GETTER_COUNT {
            return Err(missed("net_flow_getters"));
        }
        let (loss, jitter) = (flow[0], flow[2]);

        let offsets = NetChanOffsets {
            ping: imm32("net_ping_field", window, ping + 2)?,
            loss_down: imm32("net_flow_getters", networksystem, loss + 4)?,
            loss_up: imm32("net_flow_getters", networksystem, loss + 10)?,
            jitter_in: imm32("net_flow_getters", networksystem, jitter + 4)?,
            jitter_out: imm32("net_flow_getters", networksystem, jitter + 10)?,
        };
        Ok(Self { global, offsets })
    }

    /// Address of the engine global.
    pub fn global(&self) -> u64 {
        self.global
    }

    /// Derived field offsets.
    pub fn offsets(&self) -> NetChanOffsets {
        self.offsets
    }

    /// Read the current stats.
    ///
    /// # Errors
    ///
    /// [`NetChanError::Unavailable`] when the global or the channel pointer is null,
    /// [`NetChanError::Implausible`] when a field holds a value it cannot, and
    /// [`NetChanError::Memory`] when a read fails.
    pub fn read(&self, mem: &dyn MemoryReader) -> Result<NetChanStats, NetChanError> {
        let root = mem.read_u64(self.global).map_err(NetChanError::Memory)?;
        if root == 0 {
            return Err(NetChanError::Unavailable);
        }
        let chan = mem
            .read_u64(root.wrapping_add(CHAN_SLOT))
            .map_err(NetChanError::Memory)?;
        if chan == 0 {
            return Err(NetChanError::Unavailable);
        }

        let at = |off: u32| chan.wrapping_add(u64::from(off));
        let o = &self.offsets;
        let ping_ms = mem.read_i32(at(o.ping)).map_err(NetChanError::Memory)?;
        if !(0..=MAX_PING_MS).contains(&ping_ms) {
            return Err(NetChanError::Implausible {
                field: "ping",
                value: f64::from(ping_ms),
            });
        }
        let float = |field: &'static str, off: u32, max: f32| -> Result<f32, NetChanError> {
            let v = mem.read_f32(at(off)).map_err(NetChanError::Memory)?;
            if (0.0..=max).contains(&v) {
                Ok(v)
            } else {
                Err(NetChanError::Implausible {
                    field,
                    value: f64::from(v),
                })
            }
        };

        Ok(NetChanStats {
            ping_ms,
            loss_down: float("loss_down", o.loss_down, 1.0)?,
            loss_up: float("loss_up", o.loss_up, 1.0)?,
            jitter_in_ms: float("jitter_in", o.jitter_in, MAX_JITTER_MS)?,
            jitter_out_ms: float("jitter_out", o.jitter_out, MAX_JITTER_MS)?,
        })
    }
}

fn missed(name: &'static str) -> NetChanError {
    NetChanError::Unresolved(deadlock_memory::Error::SignatureNotFound(name))
}

fn find(name: &'static str, pattern: &str, image: &[u8]) -> Result<usize, NetChanError> {
    Pattern::parse(pattern)
        .map_err(NetChanError::Unresolved)?
        .find(image)
        .ok_or(missed(name))
}

fn imm32(name: &'static str, image: &[u8], at: usize) -> Result<u32, NetChanError> {
    let raw: [u8; 4] = image
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .ok_or(missed(name))?;
    let v = u32::from_le_bytes(raw);
    if v >= MAX_FIELD_OFFSET {
        return Err(missed(name));
    }
    Ok(v)
}
