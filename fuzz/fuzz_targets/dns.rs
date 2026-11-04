#![no_main]
//! Answers from the DNS-over-HTTPS resolver, parsed in the browser
//! process. Nothing may panic or loop, and every address believed must be
//! the right kind for the question.
use brickle::doh::{self, Answer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Names to ask about come first, then the answer.
    let (name, answer) = match data.iter().position(|&b| b == 0) {
        Some(i) => (String::from_utf8_lossy(&data[..i]).into_owned(), &data[i + 1..]),
        None => ("a.example".to_string(), data),
    };
    if let Some(q) = doh::query(&name, doh::TYPE_A) {
        assert_eq!(q.len() % 128, 0);
    }
    for qtype in [doh::TYPE_A, doh::TYPE_AAAA] {
        if let Some(Answer::Addrs(addrs, _)) = doh::parse(answer, &name, qtype) {
            assert!(addrs.len() <= 16);
            assert!(addrs.iter().all(|a| a.is_ipv4() == (qtype == doh::TYPE_A)));
        }
    }
});
