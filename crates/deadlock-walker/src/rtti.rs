//! MSVC RTTI: class name -> vtable address.

use memchr::memmem;

use crate::error::Result;
use crate::pe::PeImage;

/// The MSVC decorated name of a protobuf message class, as stored in its
/// `type_info`: `.?AV<Class>@<namespace>...@@`.
///
/// `package.Outer.Inner` is the C++ class `package::Outer_Inner`.
///
/// Leading segments that start lowercase are taken as the package, since a full name does
/// not say where the package ends. Valve's protos have none, and message names start
/// uppercase.
pub fn mangled_class_name(full_name: &str) -> String {
    let parts: Vec<&str> = full_name.split('.').collect();
    let split = parts
        .iter()
        .position(|p| p.starts_with(|c: char| c.is_ascii_uppercase()))
        .unwrap_or(0);
    let (package, class) = parts.split_at(split);
    let mut out = format!(".?AV{}", class.join("_"));
    for ns in package.iter().rev() {
        out.push('@');
        out.push_str(ns);
    }
    out.push_str("@@");
    out
}

/// Resolves RTTI class names to vtable addresses inside one module image.
#[derive(Debug)]
pub struct VtableResolver<'a> {
    image: &'a PeImage,
}

impl<'a> VtableResolver<'a> {
    /// Resolve against `image`.
    pub fn new(image: &'a PeImage) -> Self {
        VtableResolver { image }
    }

    /// Every primary vtable (complete-object offset 0) of the class named by a protobuf
    /// full name, as absolute addresses.
    ///
    /// The chain is: the NUL-terminated decorated name inside a `TypeDescriptor` (16 bytes
    /// in), a `CompleteObjectLocator` whose type-descriptor RVA names it, and a vtable
    /// whose slot `-1` holds that locator's address. Each link is checked, and slot 0 must
    /// point into code.
    pub fn vtables(&self, full_name: &str) -> Vec<u64> {
        let mut needle = mangled_class_name(full_name).into_bytes();
        needle.push(0);

        let mut found = Vec::new();
        for (sec_rva, sec) in self.image.data_sections() {
            for hit in memmem::find_iter(sec, &needle) {
                let Some(td_rva) = (sec_rva + hit).checked_sub(16) else {
                    continue;
                };
                if td_rva & 7 != 0 {
                    continue;
                }
                for col_rva in self.locators(td_rva as u32) {
                    self.vtables_for_locator(col_rva, &mut found);
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found
    }

    /// The vtable of the class named by a protobuf full name.
    ///
    /// # Errors
    ///
    /// [`crate::Error::ClassNotFound`] if the image has no type descriptor, no locator
    /// pointing at it, or no vtable pointing at that locator.
    pub fn vtable(&self, full_name: &str) -> Result<u64> {
        self.vtables(full_name)
            .first()
            .copied()
            .ok_or_else(|| crate::Error::ClassNotFound(full_name.to_string()))
    }

    fn locators(&self, td_rva: u32) -> Vec<u32> {
        let bytes = self.image.bytes();
        let u32_at = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        let mut out = Vec::new();
        for (sec_rva, sec) in self.image.data_sections() {
            for hit in memmem::find_iter(sec, &td_rva.to_le_bytes()) {
                let Some(col) = (sec_rva + hit).checked_sub(12) else {
                    continue;
                };
                // Signature 1 is the x64 locator and offset 0 the primary vtable. The
                // self-RVA at +20 rules out any other structure that happens to hold the
                // same RVA.
                if col & 3 == 0
                    && u32_at(col) == Some(1)
                    && u32_at(col + 4) == Some(0)
                    && u32_at(col + 20) == Some(col as u32)
                {
                    out.push(col as u32);
                }
            }
        }
        out
    }

    fn vtables_for_locator(&self, col_rva: u32, out: &mut Vec<u64>) {
        let img = self.image;
        let col_addr = (img.base() + u64::from(col_rva)).to_le_bytes();
        for (sec_rva, sec) in img.data_sections() {
            for hit in memmem::find_iter(sec, &col_addr) {
                let slot = sec_rva + hit;
                if slot & 7 != 0 {
                    continue;
                }
                let Some(first) = img.bytes().get(slot + 8..slot + 16) else {
                    continue;
                };
                if img.is_code_address(u64::from_le_bytes(first.try_into().unwrap())) {
                    out.push(img.base() + slot as u64 + 8);
                }
            }
        }
    }
}
