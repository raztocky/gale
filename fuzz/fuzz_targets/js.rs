#![no_main]
//! Scripts doing arbitrary sequences of DOM operations (the native
//! primitives in src/js.rs are what a hostile page attacks first), then
//! the result is styled, laid out and painted.
use brickle::{html, js::Runtime, page::Page};
use libfuzzer_sys::fuzz_target;
use std::time::{Duration, Instant};

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
        "const n = [document, document.documentElement, document.body];\n\
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
    let tree = html::parse(&html_src);
    let Ok(mut rt) = Runtime::new(&tree, "https://fuzz.example/", (800.0, 600.0)) else {
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
});
