//! The three `client.dll` globals the reader is built on, and how to find them.
//!
//! Each is found by AOB scan and a RIP-relative fixup; see `LAYOUT.md` §4.3.

use crate::error::Result;
use deadlock_memory::sig::{SigDesc, scan_and_resolve};

/// `CGameEntitySystem`'s identity chunk table.
///
/// Anchored on game code that indexes it:
///
/// ```text
/// movsxd rax, dword [rbx+8]
/// imul   rcx, rax, 0x130      ; sizeof(CEntityIdentity) in this build
/// mov    rax, [rip+GLOBAL]    ; <- disp32 at +14, instruction ends at +18
/// mov    rcx, [rax+rcx+...]
/// ```
pub const ENTITY_IDENTITY_LIST: SigDesc = SigDesc {
    name: "entity_identity_list",
    pattern: "48 63 43 08 48 69 C8 30 01 00 00 48 8B 05 ?? ?? ?? ?? 48 8B 8C",
    disp_off: 14,
    instr_len: 18,
};

/// The `CGameEntitySystem` instance pointer.
///
/// ```text
/// mov    rcx, [rip+GLOBAL]    ; <- disp32 at +3, ends at +7
/// xor    r9d, r9d
/// mov    byte [rsp+0x30], 1
/// ```
pub const ENTITY_SYSTEM: SigDesc = SigDesc {
    name: "entity_system",
    pattern: "48 8B 0D ?? ?? ?? ?? 45 33 C9 C6 44 24 30 01 4C",
    disp_off: 3,
    instr_len: 7,
};

/// `CSchemaSystem*`, as cached by `client.dll` itself.
///
/// ```text
/// mov    rcx, rbx
/// mov    [rip+GLOBAL], rbx    ; <- disp32 at +6, ends at +10
/// ```
///
/// Note this targets the *store* into client.dll's own cache of the pointer it received
/// from the interface factory, which is why `schemasystem.dll` never has to be touched.
pub const SCHEMA_SYSTEM: SigDesc = SigDesc {
    name: "schema_system",
    pattern: "48 8B CB 48 89 1D ?? ?? ?? ??",
    disp_off: 6,
    instr_len: 10,
};

/// The three signatures needed to locate the globals, for one target ABI.
///
/// Grouped in a struct rather than a slice so each signature stays bound to the global
/// it resolves; matching by name at the call site would be a silent-failure hazard.
#[derive(Clone, Copy, Debug)]
pub struct SignatureSet {
    /// Locates the identity chunk table reference.
    pub entity_identity_list: SigDesc,
    /// Locates the `CGameEntitySystem` instance pointer.
    pub entity_system: SigDesc,
    /// Locates client's cached `CSchemaSystem*`.
    pub schema_system: SigDesc,
}

impl SignatureSet {
    /// All three, for iteration.
    pub fn all(&self) -> [SigDesc; 3] {
        [
            self.entity_identity_list,
            self.entity_system,
            self.schema_system,
        ]
    }
}

/// Signatures for the MSVC/PE client - the shipping build, on Windows or under Proton.
pub const WIN64_SIGNATURES: SignatureSet = SignatureSet {
    entity_identity_list: ENTITY_IDENTITY_LIST,
    entity_system: ENTITY_SYSTEM,
    schema_system: SCHEMA_SYSTEM,
};

/// All three signatures, in the order the original table lists them.
///
/// Win64 only; use [`crate::abi::Abi::signatures`] to pick a set by target ABI.
pub const SIGNATURES: [SigDesc; 3] = [ENTITY_IDENTITY_LIST, ENTITY_SYSTEM, SCHEMA_SYSTEM];

/// Addresses of the three globals, in the target process.
///
/// These are the addresses *of the variables*, not of what they point to; dereference
/// them with a further read.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Globals {
    /// Address of the identity chunk table pointer.
    pub entity_identity_list: u64,
    /// Address of the `CGameEntitySystem*` variable.
    pub entity_system: u64,
    /// Address of the `CSchemaSystem*` variable.
    pub schema_system: u64,
}

impl std::fmt::Debug for Globals {
    /// Addresses are only ever useful in hex.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Globals")
            .field(
                "entity_identity_list",
                &format_args!("{:#x}", self.entity_identity_list),
            )
            .field("entity_system", &format_args!("{:#x}", self.entity_system))
            .field("schema_system", &format_args!("{:#x}", self.schema_system))
            .finish()
    }
}

impl Globals {
    /// Scan a client image copy with an explicit signature set.
    ///
    /// `image` must start at `module_base`. Any signature failing is fatal: a partial
    /// resolution is not useful and silently continuing would produce garbage reads.
    pub fn resolve_with(
        set: &SignatureSet,
        image: &[u8],
        module_base: u64,
        module_size: usize,
    ) -> Result<Self> {
        Ok(Globals {
            entity_identity_list: scan_and_resolve(
                &set.entity_identity_list,
                image,
                module_base,
                module_size,
            )?,
            entity_system: scan_and_resolve(&set.entity_system, image, module_base, module_size)?,
            schema_system: scan_and_resolve(&set.schema_system, image, module_base, module_size)?,
        })
    }

    /// Scan a `client.dll` image copy using the Win64 signature set.
    pub fn resolve(image: &[u8], module_base: u64, module_size: usize) -> Result<Self> {
        Self::resolve_with(&WIN64_SIGNATURES, image, module_base, module_size)
    }

    /// Resolve each signature independently, reporting per-signature failures.
    ///
    /// Useful for diagnostics when a game update breaks exactly one pattern.
    pub fn resolve_partial(
        set: &SignatureSet,
        image: &[u8],
        module_base: u64,
        module_size: usize,
    ) -> [(&'static str, deadlock_memory::Result<u64>); 3] {
        set.all().map(|d| {
            (
                d.name,
                scan_and_resolve(&d, image, module_base, module_size),
            )
        })
    }
}
