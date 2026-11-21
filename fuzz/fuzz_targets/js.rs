#![no_main]
//! Scripts doing arbitrary sequences of DOM operations (the native
//! primitives in src/js.rs are what a hostile page attacks first), then
//! the result is styled, laid out and painted.
//!
//! Parsing the prelude is most of what a new runtime costs, so one runtime
//! is kept and reset for each input (see `Runtime::reset_for_fuzzing`),
//! about five times as many inputs a second. It's built anew every
//! [`FRESH_EVERY`] inputs: Boa 0.22 keeps every prototype that has had
//! instances alive for the engine's lifetime, so a kept engine slowly
//! fills up with old pages. The operations below never touch the engine's
//! built-in objects, which a reset doesn't restore; the script is a block
//! so its declarations don't outlive it.
//!
//! A crash that only happens after earlier inputs may not reproduce on its
//! own; run the inputs before it too (`-runs` over the corpus).
use brickle::{html, js::Runtime, page::Page};
use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

const FRESH_EVERY: u32 = 50;

/// No leak checks for this target: the kept runtime's strings and objects
/// are held through Boa's tagged pointers, which LeakSanitizer can't follow,
/// so it reports them as leaked after nearly every input. (The other targets
/// still check for leaks.)
#[no_mangle]
pub extern "C" fn __asan_default_options() -> *const std::ffi::c_char {
    c"detect_leaks=0".as_ptr()
}

thread_local! {
    // Never dropped at exit, when the engine's own thread-locals may
    // already be gone; replaced runtimes are dropped as usual.
    static KEPT: ManuallyDrop<RefCell<(Option<Runtime>, u32)>> =
        const { ManuallyDrop::new(RefCell::new((None, 0))) };
}

/// A runtime for `tree`: the kept one reset, or a new one.
fn runtime(tree: &brickle::dom::Node) -> Option<Runtime> {
    const URL: &str = "https://fuzz.example/";
    KEPT.with(|k| {
        let (kept, uses) = &mut *k.borrow_mut();
        *uses += 1;
        if *uses % FRESH_EVERY != 0 {
            if let Some(mut rt) = kept.take() {
                if rt.reset_for_fuzzing(tree, URL, (800.0, 600.0)).is_ok() {
                    return Some(rt);
                }
            }
        }
        *kept = None;
        Runtime::new(tree, URL, (800.0, 600.0)).ok()
    })
}

const OPS: &[&str] = &[
    "n.push(document.createElement(['div','p','b','script','style','ul','li','a','#x'][r()%9]))",
    "n.push(document.createTextNode('t' + r()))",
    "n.push(document.createDocumentFragment())",
    "pick().appendChild(pick())",
    "pick().insertBefore(pick(), pick())",
    "pick().insertBefore(pick(), null)",
    "pick().removeChild(pick())",
    "pick().remove()",
    "pick().replaceWith(pick(), 'txt')",
    "pick().replaceChild(pick(), pick())",
    "pick().append(pick(), 's', pick())",
    "pick().prepend(pick())",
    "pick().before(pick())",
    "pick().after(pick())",
    "pick().innerHTML = ['<p><b>x</b></p>', '<table><tr><td>', '<script>x</script>', '', '<<>>&amp;'][r()%5]",
    "pick().outerHTML = '<i>o</i>'",
    "pick().insertAdjacentHTML(['beforebegin','afterbegin','beforeend','afterend'][r()%4], '<u>u</u>')",
    "pick().textContent = 'y'.repeat(r())",
    "pick().textContent",
    "pick().innerHTML",
    "pick().outerHTML",
    "n.push(pick().cloneNode(!!(r()%2)))",
    "pick().setAttribute(['class','id','style','href','onclick','data-x'][r()%6], 'a b ' + r())",
    "pick().removeAttribute('class')",
    "pick().classList.toggle('a')",
    "pick().style.color = 'red'",
    "pick().dataset.fooBar = r()",
    "pick().id = 'i' + (r()%3)",
    "n.push(document.getElementById('i' + (r()%3)))",
    "n.push(...document.querySelectorAll(['p b', '.a > div', '*', 'li', '#i1', '[data-foo-bar]', 'a, b'][r()%7]))",
    "n.push(pick().closest('div'))",
    "pick().matches('div p')",
    "n.push(pick().parentNode, pick().firstChild, pick().nextSibling, pick().lastElementChild)",
    "n.push(...pick().childNodes)",
    "pick().contains(pick())",
    "pick().click()",
    "pick().addEventListener('click', (e) => { e.target.remove(); e.preventDefault(); })",
    "pick().dispatchEvent(new CustomEvent('x', {bubbles: true}))",
    "setTimeout(() => pick().remove(), r())",
    "setInterval(() => pick().appendChild(pick()), 1)",
    "requestAnimationFrame(() => pick().textContent = 'raf')",
    "import(['./m.js', 'bare', 'https://x.example/a.js', ':bad', ''][r()%5]).catch(() => {})",
    "document.write('<em>w</em>')",
    "document.title = 'T' + r()",
    "pick().getBoundingClientRect()",
    "location.hash = 'h' + r()",
];

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let split = (data[0] as usize) % data.len().min(512);
    let html_src = String::from_utf8_lossy(&data[1..1 + split.min(data.len() - 1)]);
    let ops = &data[1 + split.min(data.len() - 1)..];
    let mut script = String::from(
        "{ const n = [document, document.documentElement, document.body];\n\
         let k = 0; const seq = [",
    );
    for b in ops {
        script.push_str(&format!("{b},"));
    }
    script.push_str(
        "0];\nconst r = () => seq[k++ % seq.length];\n\
         const pick = () => n[r() % n.length] || document.body;\n",
    );
    for (i, b) in ops.iter().enumerate().take(400) {
        let op = OPS[*b as usize % OPS.len()];
        script.push_str(&format!("try {{ {op} }} catch (e) {{}} // {i}\n"));
    }
    script.push('}');
    let tree = html::parse(&html_src);
    let Some(mut rt) = runtime(&tree) else {
        return;
    };
    rt.run_script(&script, 0, "fuzz");
    rt.fire_load();
    rt.run_timers(Instant::now() + Duration::from_millis(200));
    let tree = rt.tree();
    let mut out = String::new();
    tree.dump(0, &mut out);
    let page = Page::from_tree("https://fuzz.example/", tree);
    let r = page.render(800.0, 600.0);
    assert!(r.height.is_finite());
    if let Some(uid) = page.hit(10.0, 10.0) {
        let _ = rt.click(uid, Instant::now() + Duration::from_millis(100));
    }
    KEPT.with(|k| k.borrow_mut().0 = Some(rt));
});
