#![no_main]
//! The address bar and find bar: any sequence of typing, pasting, moving,
//! selecting and deleting keeps the cursor and selection inside the text,
//! on character boundaries, and the text within its length limit.
use brickle::lineedit::LineEdit;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut e = LineEdit::new(64);
    let pieces = ["a", "é", "中文", "🐈", " ", "x.com/", "ẞ", "\u{301}"];
    for &op in data {
        let (word, extend) = (op & 0x10 != 0, op & 0x20 != 0);
        match op % 12 {
            0 => e.insert(pieces[(op as usize / 12) % pieces.len()]),
            1 => e.backspace(word),
            2 => e.delete(word),
            3 => e.left(word, extend),
            4 => e.right(word, extend),
            5 => e.move_to(op as usize / 12, extend),
            6 => e.select_all(),
            7 => {
                let _ = e.cut();
            }
            8 => e.set(&"é".repeat(op as usize / 4)),
            9 => {
                let _ = e.selected();
            }
            10 => e.insert(&pieces.concat()),
            _ => e.move_to(usize::MAX, extend),
        }
        let n = e.text.chars().count();
        assert!(e.text.len() <= e.max_len);
        assert!(e.cursor <= n);
        assert!(e.anchor.is_none_or(|a| a <= n));
        if let Some((a, b)) = e.selection() {
            assert!(a < b && b <= n);
            assert_eq!(e.selected().map(|s| s.chars().count()), Some(b - a));
        }
    }
});
