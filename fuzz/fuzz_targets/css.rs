#![no_main]
//! The CSS parser must accept any input without panicking.
use brickle::css;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let src = String::from_utf8_lossy(data);
    let sheet = css::parse(&src);
    assert!(sheet.rules.len() <= css::MAX_RULES);
    for rule in &sheet.rules {
        for sel in &rule.selectors {
            let _ = sel.specificity();
        }
    }
    let _ = css::parse_inline(&src);
});
