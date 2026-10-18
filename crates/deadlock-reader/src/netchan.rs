use std::net::{Ipv4Addr, SocketAddrV4};

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

/// `CNetChan::IsLoopback`: `sub rsp,28h; add rcx,REMOTE; cmp dword [rcx+1Ch],0; jnz ..;
/// call [netadr_t::IsLocalhost]`. The imm32 at +7 is the remote `ns_address`.
const REMOTE_GETTER: &str = "48 83 EC 28 48 81 C1 ?? ?? ?? ?? 83 79 1C 00 75 ?? FF 15";

/// In `CNetworkGameClient::SetSignonState`: `cmp edi,[rsi+SIGNON]; jg ..;
/// mov rcx,[rsi+0F0h]`. The disp32 at +2 is the signon state, on the object whose
/// `+0xF0` is the channel [`ENGINE_GLOBAL`] leads to.
const SIGNON_FIELD: &str = "3B BE ?? ?? ?? ?? 7F ?? 48 8B 8E F0 00 00 00";

/// `ns_address`: a `netadr_t` at `+0x0`, a packed `CSteamID` at `+0xc`, the address kind
/// at `+0x1c`. `netadr_t` is `{ u32 type; u8 ip[4]; u16 port_be; }`.
const NS_ADDRESS_LEN: usize = 0x20;
const NS_STEAM_ID: usize = 0xc;
const NS_KIND: usize = 0x1c;
const NETADR_TYPE_LOOPBACK: u32 = 1;

/// `SetSignonState` rejects anything above `SIGNONSTATE_CHANGELEVEL`.
const MAX_SIGNON: u32 = 7;

/// `SIGNONSTATE_FULL` in the engine's `SignonState_t`: the client is fully in the game.
pub const SIGNON_FULL: u32 = 6;

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

/// Offsets for [`NetChanAnchors::read_connection`]. `None` where a signature missed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionOffsets {
    /// The remote `ns_address` inside `CNetChan`.
    pub remote: Option<u32>,
    /// The signon state, `u32`, inside the object the engine global points to.
    pub signon: Option<u32>,
}

/// The server end of the net channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Remote {
    /// A Steam networking peer: the server's 64-bit SteamID.
    SteamId(u64),
    /// A plain IP peer. `None` when the address is unset.
    Ip(Option<SocketAddrV4>),
    /// This machine.
    Loopback,
    /// An address kind this crate does not decode, as the engine numbers it.
    Unknown(u32),
}

/// What the net channel says about the current connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NetConnection {
    /// Raw `SignonState_t`, 0..=7. `None` when its offset could not be derived.
    pub signon: Option<u32>,
    /// `None` when its offset could not be derived.
    pub remote: Option<Remote>,
}

impl NetConnection {
    /// The client has finished signing on and is in the game.
    pub fn is_full(&self) -> bool {
        self.signon == Some(SIGNON_FULL)
    }
}

/// Where the net channel lives and how it is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetChanAnchors {
    global: u64,
    offsets: NetChanOffsets,
    connection: ConnectionOffsets,
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
        let connection = ConnectionOffsets {
            remote: find("net_remote_getter", REMOTE_GETTER, networksystem)
                .and_then(|at| imm32("net_remote_getter", networksystem, at + 7))
                .ok(),
            signon: find("net_signon_field", SIGNON_FIELD, engine)
                .and_then(|at| imm32("net_signon_field", engine, at + 2))
                .ok(),
        };
        Ok(Self {
            global,
            offsets,
            connection,
        })
    }

    /// Address of the engine global.
    pub fn global(&self) -> u64 {
        self.global
    }

    /// Derived field offsets.
    pub fn offsets(&self) -> NetChanOffsets {
        self.offsets
    }

    /// Derived offsets for [`NetChanAnchors::read_connection`].
    pub fn connection_offsets(&self) -> ConnectionOffsets {
        self.connection
    }

    /// Read the signon state and the server end of the channel.
    ///
    /// `Ok(None)` when the global or the channel pointer is null: not connected.
    ///
    /// # Errors
    ///
    /// [`NetChanError::Implausible`] when the signon state is out of range, and
    /// [`NetChanError::Memory`] when a read fails.
    pub fn read_connection(
        &self,
        mem: &dyn MemoryReader,
    ) -> Result<Option<NetConnection>, NetChanError> {
        let Some((root, chan)) = self.channel(mem)? else {
            return Ok(None);
        };
        let signon = match self.connection.signon {
            Some(off) => {
                let v = mem
                    .read_u32(root.wrapping_add(u64::from(off)))
                    .map_err(NetChanError::Memory)?;
                if v > MAX_SIGNON {
                    return Err(NetChanError::Implausible {
                        field: "signon",
                        value: f64::from(v),
                    });
                }
                Some(v)
            }
            None => None,
        };
        let remote = match self.connection.remote {
            Some(off) => {
                let mut ns = [0u8; NS_ADDRESS_LEN];
                mem.read_into(chan.wrapping_add(u64::from(off)), &mut ns)
                    .map_err(NetChanError::Memory)?;
                Some(decode_remote(&ns))
            }
            None => None,
        };
        Ok(Some(NetConnection { signon, remote }))
    }

    /// The engine global's object and its channel, or `None` when either pointer is null.
    fn channel(&self, mem: &dyn MemoryReader) -> Result<Option<(u64, u64)>, NetChanError> {
        let root = mem.read_u64(self.global).map_err(NetChanError::Memory)?;
        if root == 0 {
            return Ok(None);
        }
        let chan = mem
            .read_u64(root.wrapping_add(CHAN_SLOT))
            .map_err(NetChanError::Memory)?;
        Ok((chan != 0).then_some((root, chan)))
    }

    /// Read the current stats.
    ///
    /// # Errors
    ///
    /// [`NetChanError::Unavailable`] when the global or the channel pointer is null,
    /// [`NetChanError::Implausible`] when a field holds a value it cannot, and
    /// [`NetChanError::Memory`] when a read fails.
    pub fn read(&self, mem: &dyn MemoryReader) -> Result<NetChanStats, NetChanError> {
        let (_, chan) = self.channel(mem)?.ok_or(NetChanError::Unavailable)?;

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

fn decode_remote(ns: &[u8; NS_ADDRESS_LEN]) -> Remote {
    let le32 = |at: usize| u32::from_le_bytes([ns[at], ns[at + 1], ns[at + 2], ns[at + 3]]);
    match le32(NS_KIND) {
        0 => {
            let adr_type = le32(0);
            let ip = Ipv4Addr::new(ns[4], ns[5], ns[6], ns[7]);
            let port = u16::from_be_bytes([ns[8], ns[9]]);
            // `CNetChan::IsLoopback` asks `netadr_t::IsLocalhost`, which is 127.0.0.1 only;
            // `netadr_t::IsLoopback` is the type alone. Either means this machine.
            if ip == Ipv4Addr::LOCALHOST || adr_type == NETADR_TYPE_LOOPBACK {
                Remote::Loopback
            } else if adr_type == 0 || ip.is_unspecified() || port == 0 {
                // What `netadr_t::IsValid` rejects.
                Remote::Ip(None)
            } else {
                Remote::Ip(Some(SocketAddrV4::new(ip, port)))
            }
        }
        1..=3 => {
            let lo = u64::from(le32(NS_STEAM_ID));
            let hi = u64::from(le32(NS_STEAM_ID + 4));
            Remote::SteamId(hi << 32 | lo)
        }
        kind => Remote::Unknown(kind),
    }
}

/// Whether resolving would miss again on the same process: the images are already loaded,
/// so a signature miss is a property of this build, not of timing.
pub(crate) fn is_permanent(err: &NetChanError) -> bool {
    matches!(
        err,
        NetChanError::Unresolved(
            deadlock_memory::Error::SignatureNotFound(_)
                | deadlock_memory::Error::SignatureTruncated { .. }
                | deadlock_memory::Error::OutsideModule { .. }
                | deadlock_memory::Error::BadPattern(_)
        )
    )
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
