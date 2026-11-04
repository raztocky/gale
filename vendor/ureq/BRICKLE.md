# ureq 2.12.1, patched for Brickle

An unmodified copy of ureq 2.12.1 from crates.io except for `src/header.rs` (and an
`#![allow(warnings)]` in `src/lib.rs`, since newer compilers warn about its code). A header line
with no `:` used to be accepted with its name running to the end of the line. Reading its
value then sliced past the end and panicked, so any server could crash a request. Found by
Brickle's `response` fuzz target. Now such lines are rejected (responses skip them) and the
slicing can't go out of range.

ureq 2.12.1 is the last 2.x release. This copy goes away when Brickle moves to ureq 3.
It's wired in with `[patch.crates-io]` in the top-level `Cargo.toml`.
