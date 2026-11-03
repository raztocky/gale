#![no_main]
//! The HTML parser must accept any input without panicking, and must respect
//! its depth/size caps.
use brickle::html;
use libfuzzer_sys::fuzz_target;

fn depth(n: &brickle::dom::Node) -> usize {
    1 + n.children.iter().map(depth).max().unwrap_or(0)
}

fuzz_target!(|data: &[u8]| {
    let src = String::from_utf8_lossy(data);
    let doc = html::parse(&src);
    assert!(depth(&doc) <= html::MAX_DEPTH + 3);
});
