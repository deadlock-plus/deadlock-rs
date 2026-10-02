//! Sized reads are capped before anything is allocated.

use deadlock_memory::Error;
use deadlock_memory::mem::{MAX_READ_BYTES, MemoryReader};
use deadlock_memory::mock::MockMemory;

/// The obvious use of a sized read is a length field taken out of the target process, so a
/// corrupt one has to be refused rather than allocated.
#[test]
fn an_absurd_read_length_is_refused_before_anything_is_allocated() {
    let m = MockMemory::new(1);
    let err = m.read_bytes(0x1000, MAX_READ_BYTES + 1).unwrap_err();
    assert!(
        matches!(err, Error::ReadTooLarge { .. }),
        "expected a refusal, got {err:?}"
    );
    let err = m.read_bytes(0x1000, MAX_READ_BYTES).unwrap_err();
    assert!(
        !matches!(err, Error::ReadTooLarge { .. }),
        "the cap is inclusive, so this must fail on the read instead: {err:?}"
    );
}
