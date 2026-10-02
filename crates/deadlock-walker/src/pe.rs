//! A module image copied out of the target, with its PE section table.

use deadlock_memory::mem::{MemoryReader, Module};

use crate::error::{Error, Result};

const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

/// One PE section, as mapped in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// Section name, e.g. `.rdata`.
    pub name: String,
    /// Offset from the image base.
    pub rva: u32,
    /// Size in memory.
    pub size: u32,
    /// `IMAGE_SCN_*` flags.
    pub characteristics: u32,
}

impl Section {
    /// Whether the loader maps this section executable.
    pub fn is_code(&self) -> bool {
        self.characteristics & IMAGE_SCN_MEM_EXECUTE != 0
    }
}

/// A loaded PE64 image, copied into local memory.
#[derive(Debug)]
pub struct PeImage {
    base: u64,
    bytes: Vec<u8>,
    sections: Vec<Section>,
}

impl PeImage {
    /// Copy `module` out of the target and parse its headers.
    ///
    /// # Errors
    ///
    /// [`Error::BadImage`] if nothing could be read or the headers do not parse. Pages that
    /// fail to read come back zeroed rather than failing the copy.
    pub fn read(mem: &dyn MemoryReader, module: &Module) -> Result<Self> {
        let (bytes, got) = mem.read_image(module.base, module.size);
        if got == 0 {
            return Err(Error::BadImage("module image is unreadable"));
        }
        Self::from_bytes(module.base, bytes)
    }

    /// Parse an image already in local memory, mapped at `base`.
    ///
    /// # Errors
    ///
    /// [`Error::BadImage`] unless the bytes carry a PE32+ x64 header and a section table.
    pub fn from_bytes(base: u64, bytes: Vec<u8>) -> Result<Self> {
        let u16_at = |at: usize| -> Option<u16> {
            Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
        };
        let u32_at = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        if bytes.get(..2) != Some(b"MZ".as_slice()) {
            return Err(Error::BadImage("missing MZ header"));
        }
        let nt = u32_at(0x3c).ok_or(Error::BadImage("truncated DOS header"))? as usize;
        if bytes.get(nt..nt + 4) != Some(b"PE\0\0".as_slice()) {
            return Err(Error::BadImage("missing PE signature"));
        }
        if u16_at(nt + 4) != Some(0x8664) {
            return Err(Error::BadImage("not an x64 image"));
        }
        let count = u16_at(nt + 6).ok_or(Error::BadImage("truncated COFF header"))? as usize;
        let opt_size = u16_at(nt + 20).ok_or(Error::BadImage("truncated COFF header"))? as usize;
        let opt = nt + 24;
        if u16_at(opt) != Some(0x20b) {
            return Err(Error::BadImage("not a PE32+ optional header"));
        }
        let mut sections = Vec::with_capacity(count);
        for i in 0..count {
            let at = opt + opt_size + i * 40;
            let truncated = Error::BadImage("truncated section table");
            let name = bytes.get(at..at + 8).ok_or(truncated)?;
            let name_len = name.iter().position(|&b| b == 0).unwrap_or(8);
            let field =
                |off: usize| u32_at(at + off).ok_or(Error::BadImage("truncated section table"));
            sections.push(Section {
                name: String::from_utf8_lossy(&name[..name_len]).into_owned(),
                size: field(8)?,
                rva: field(12)?,
                characteristics: field(36)?,
            });
        }
        Ok(PeImage {
            base,
            bytes,
            sections,
        })
    }

    /// Address the image is mapped at in the target.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// The image bytes, indexed by RVA.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Section table.
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Whether `addr` (absolute) falls in an executable section.
    pub fn is_code_address(&self, addr: u64) -> bool {
        let Some(rva) = addr.checked_sub(self.base) else {
            return false;
        };
        self.sections.iter().any(|s| {
            s.is_code() && rva >= u64::from(s.rva) && rva - u64::from(s.rva) < u64::from(s.size)
        })
    }

    /// The bytes of every non-executable section, with the RVA each starts at.
    pub(crate) fn data_sections(&self) -> impl Iterator<Item = (usize, &[u8])> {
        self.sections
            .iter()
            .filter(|s| !s.is_code())
            .filter_map(|s| {
                let start = s.rva as usize;
                let end = (start + s.size as usize).min(self.bytes.len());
                Some((start, self.bytes.get(start..end)?))
            })
    }
}
