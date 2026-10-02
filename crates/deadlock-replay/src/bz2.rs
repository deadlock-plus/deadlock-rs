//! bzip2 decompression for the `.meta.bz2` the replay CDN serves.
//!
//! Pure Rust, via `bzip2-rs`. The `libbz2` bindings would want a C toolchain, which the
//! cross-compile job does not have and the workspace does not require of anyone building
//! it.

use bzip2_rs::decoder::{Decoder, ReadState, WriteState};

use crate::error::{Error, Result};

/// Ceiling [`decompress`] applies, in bytes.
///
/// A real match's metadata runs to a few megabytes. This is the order of magnitude above
/// that, chosen so a stream that expands without bound is refused long before it matters
/// and a genuine file never is.
pub const DEFAULT_MAX_DECOMPRESSED: usize = 64 * 1024 * 1024;

/// Working buffer per read. One decoded block is 100-900 KB, so this is a chunk size, not
/// a limit.
const CHUNK: usize = 32 * 1024;

/// Rounds the decoder may make no progress before the stream is called truncated.
///
/// Ending a stream legitimately costs two such rounds: one that writes the empty buffer
/// signalling end of input, and one that reads the resulting `Eof`. Anything beyond that
/// is a decoder asking for input that will never arrive, which on a fixed input slice can
/// only spin.
const MAX_STALLED_ROUNDS: u32 = 4;

/// The stall guard has to stay small enough to be a guard.
///
/// [`MAX_STALLED_ROUNDS`] is what stops a decoder that asks for input which will never
/// arrive from spinning on a fixed slice — `bzip2-rs`'s own example loops forever on an
/// empty file for exactly this reason. Raising it does not break correctness, it just
/// makes the guard take proportionally longer to fire, and at a large enough value it
/// stops being a guard at all.
///
/// Ending a stream legitimately costs two rounds, so three is the floor below which real
/// streams would be refused.
const _: () = assert!(MAX_STALLED_ROUNDS >= 3);
const _: () = assert!(MAX_STALLED_ROUNDS <= 64);

/// The decompression ceiling has to clear a real file and still bound a hostile one.
///
/// A real match's metadata runs to a few megabytes, so anything at or below that would
/// refuse genuine files — and since no `.meta.bz2` exists in this workspace to test
/// against, nothing else would notice. Sixteen mebibytes is the floor that keeps a real
/// file safe; the ceiling keeps the cap meaningful as a cap.
const _: () = assert!(DEFAULT_MAX_DECOMPRESSED >= 16 * 1024 * 1024);
const _: () = assert!(DEFAULT_MAX_DECOMPRESSED <= 512 * 1024 * 1024);

/// Whether the payload opens with a bzip2 header.
///
/// `BZ` signature, `h` for the Huffman format, then the block size as an ASCII digit.
fn has_bzip2_header(bytes: &[u8]) -> bool {
    matches!(bytes, [b'B', b'Z', b'h', size, ..] if size.is_ascii_digit() && *size != b'0')
}

/// Decompress a bzip2 stream, refusing anything over [`DEFAULT_MAX_DECOMPRESSED`].
pub fn decompress(bytes: &[u8]) -> Result<Vec<u8>> {
    decompress_capped(bytes, DEFAULT_MAX_DECOMPRESSED)
}

/// Decompress a bzip2 stream, refusing anything over `cap` bytes.
///
/// The cap is checked as output accumulates rather than afterwards, so a stream that
/// expands to gigabytes costs the cap plus one chunk of memory and no more.
pub fn decompress_capped(bytes: &[u8], cap: usize) -> Result<Vec<u8>> {
    // Checked here rather than left to the decoder because the failure this catches is
    // usually not a corrupt stream: it is the CDN handing back an error page, or a caller
    // passing the bytes of a `.dem` by mistake.
    if !has_bzip2_header(bytes) {
        return Err(Error::NotBzip2);
    }

    let mut decoder = Decoder::new();
    let mut input = bytes;
    let mut out = Vec::new();
    let mut buf = [0u8; CHUNK];
    let mut stalled = 0;

    loop {
        match decoder.read(&mut buf).map_err(|e| bzip2_error(&e))? {
            ReadState::Read(n) => {
                if out.len() + n > cap {
                    return Err(Error::TooLarge { cap });
                }
                out.extend_from_slice(&buf[..n]);
                stalled = 0;
            }
            ReadState::NeedsWrite(_) => {
                // An empty `input` here is not an error: it is how the decoder is told
                // the last block is short and can be decoded with what it already holds.
                match decoder.write(input).map_err(|e| bzip2_error(&e))? {
                    WriteState::NeedsRead => {}
                    WriteState::Written(n) => {
                        input = &input[n..];
                        if n > 0 {
                            stalled = 0;
                        }
                    }
                }
                stalled += 1;
                if stalled > MAX_STALLED_ROUNDS {
                    return Err(Error::TruncatedStream);
                }
            }
            ReadState::Eof => return Ok(out),
        }
    }
}

fn bzip2_error(e: &bzip2_rs::decoder::DecoderError) -> Error {
    Error::Bzip2(e.to_string())
}
