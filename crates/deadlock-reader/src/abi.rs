//! Which binary the reader is looking at, and the constants that follow from it.
//!
//! # Why this is a runtime choice, not `#[cfg]`
//!
//! Signatures, fallback field offsets and the schema/entity struct layouts all depend on
//! how the **target** was compiled, not on the host the reader runs on. A Linux build
//! reading a Proton-hosted game needs the *Windows* tables, because Wine maps the real
//! PE and the game code is still MSVC-compiled x86-64. The same Linux build reading a
//! future native `client.so` would need `SysV` tables.
//!
//! So the ABI is detected from the image that was actually mapped, and everything
//! ABI-dependent hangs off [`Abi`].

use crate::entity::EntityLayout;
use crate::globals::SignatureSet;
use crate::schema::SchemaLayout;

/// The ABI of the target module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Abi {
    /// PE image, MSVC C++ ABI, Win64 calling convention.
    ///
    /// The shipping Deadlock client, whether run on Windows or under Proton/Wine.
    Win64,
    /// ELF image, Itanium C++ ABI, System V calling convention.
    ///
    /// What a native Linux build would be. No constants have been derived for it yet -
    /// see [`Abi::is_supported`].
    SysV,
}

impl Abi {
    /// Detect from the first bytes of a mapped image.
    ///
    /// `MZ` for PE, `\x7fELF` for ELF. This is the authoritative check: it describes
    /// what is really in memory, regardless of file extension or host OS.
    pub fn detect(image: &[u8]) -> Option<Abi> {
        match image {
            [0x4D, 0x5A, ..] => Some(Abi::Win64),
            [0x7F, 0x45, 0x4C, 0x46, ..] => Some(Abi::SysV),
            _ => None,
        }
    }

    /// Guess from a module file name, for use before anything is mapped.
    ///
    /// Weaker than [`Abi::detect`]; prefer that when an image is available.
    pub fn from_module_name(name: &str) -> Option<Abi> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".dll") {
            Some(Abi::Win64)
        } else if lower.ends_with(".so") || lower.contains(".so.") {
            Some(Abi::SysV)
        } else {
            None
        }
    }

    /// The client module name to look for.
    pub fn client_module(&self) -> &'static str {
        match self {
            Abi::Win64 => "client.dll",
            Abi::SysV => "client.so",
        }
    }

    /// Whether the constants needed to read this ABI have been derived.
    ///
    /// `SysV` is currently `false`: Deadlock ships no native Linux build, so its
    /// signatures and struct layouts have never been observed. Everything is in place
    /// for it - see the porting notes in `README.md`.
    pub fn is_supported(&self) -> bool {
        self.signatures().is_some()
    }

    /// Signatures for locating the three globals.
    pub fn signatures(&self) -> Option<&'static SignatureSet> {
        match self {
            Abi::Win64 => Some(&crate::globals::WIN64_SIGNATURES),
            Abi::SysV => None,
        }
    }

    /// Hardcoded field offset, used only when the runtime schema lookup misses.
    pub fn fallback_offset(&self, class: &str, field: &str) -> Option<u32> {
        match self {
            Abi::Win64 => crate::fields::win64_fallback_offset(class, field),
            // Itanium ABI packs classes differently, so the MSVC table would be actively
            // wrong here. Better to return nothing and rely on the runtime schema.
            Abi::SysV => None,
        }
    }

    /// Schema-system struct layout.
    pub fn schema_layout(&self) -> Option<SchemaLayout> {
        match self {
            Abi::Win64 => Some(SchemaLayout::DEADLOCK),
            Abi::SysV => None,
        }
    }

    /// Entity-identity struct layout.
    pub fn entity_layout(&self) -> Option<EntityLayout> {
        match self {
            Abi::Win64 => Some(EntityLayout::DEADLOCK),
            Abi::SysV => None,
        }
    }

    /// Human-readable name.
    pub fn name(&self) -> &'static str {
        match self {
            Abi::Win64 => "Win64/PE (MSVC)",
            Abi::SysV => "SysV/ELF (Itanium)",
        }
    }
}

impl std::fmt::Display for Abi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_pe_and_elf() {
        assert_eq!(Abi::detect(b"MZ\x90\x00rest"), Some(Abi::Win64));
        assert_eq!(Abi::detect(b"\x7fELF\x02\x01"), Some(Abi::SysV));
        assert_eq!(Abi::detect(b"nope"), None);
        assert_eq!(Abi::detect(b"M"), None);
        assert_eq!(Abi::detect(b""), None);
    }

    #[test]
    fn guesses_from_module_name() {
        assert_eq!(Abi::from_module_name("client.dll"), Some(Abi::Win64));
        assert_eq!(Abi::from_module_name("CLIENT.DLL"), Some(Abi::Win64));
        assert_eq!(Abi::from_module_name("client.so"), Some(Abi::SysV));
        assert_eq!(Abi::from_module_name("libc.so.6"), Some(Abi::SysV));
        assert_eq!(Abi::from_module_name("deadlock"), None);
    }

    /// The whole point of this module: a Proton game is Win64 even on a Linux host.
    #[test]
    fn proton_target_is_win64_regardless_of_host() {
        let pe_header = b"MZ\x90\x00\x03\x00\x00\x00";
        let abi = Abi::detect(pe_header).unwrap();
        assert_eq!(abi, Abi::Win64);
        assert!(abi.is_supported());
        assert_eq!(
            abi.fallback_offset("C_CitadelGameRulesProxy", "m_pGameRules"),
            Some(0x5f0)
        );
        assert_eq!(abi.schema_layout().unwrap().system_type_scopes, 0x190);
        assert_eq!(abi.entity_layout().unwrap().identity_stride, 0x70);
    }

    /// A native build must fail loudly rather than silently using MSVC offsets.
    #[test]
    fn sysv_reports_unsupported_rather_than_guessing() {
        let abi = Abi::SysV;
        assert!(!abi.is_supported());
        assert!(abi.signatures().is_none());
        assert!(abi.schema_layout().is_none());
        assert!(abi.entity_layout().is_none());
        assert_eq!(
            abi.fallback_offset("C_CitadelGameRulesProxy", "m_pGameRules"),
            None
        );
    }
}
