//! Any bytes the tree decoder accepts must re-encode to exactly the same bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use roub_core::Tree;

fuzz_target!(|data: &[u8]| {
    if let Ok(tree) = Tree::decode(data) {
        assert_eq!(tree.encode(), data, "accepted a non-canonical tree");
    }
});
