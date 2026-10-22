//! Any bytes the changeset decoder accepts must re-encode to exactly the same bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use roub_core::Changeset;

fuzz_target!(|data: &[u8]| {
    if let Ok(changeset) = Changeset::decode(data) {
        assert_eq!(
            changeset.encode(),
            data,
            "accepted a non-canonical changeset"
        );
    }
});
