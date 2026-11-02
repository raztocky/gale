mod css;
mod dom;
mod font;
#[cfg(feature = "gui")]
mod gui;
mod html;
mod layout;
mod net;
mod page;
mod paint;
mod style;

use page::Page;

const USAGE: &str = "\
brickle — Brickle Browser, a tiny from-scratch browser engine

USAGE:
  brickle [URL|FILE]                        open a browser window
  brickle render URL|FILE [-o out.png] [-w WIDTH]   render a full page to PNG
  brickle dom URL|FILE                      print the parsed DOM tree
";

fn main() {
    // Style, layout and paint recurse over the tree. The parser caps nesting
    // depth, and a roomy stack gives that cap a wide safety margin.
    let worker = std::thread::Builder::new()
        .name("brickle".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(real_main)
        .expect("failed to start main thread");
    if worker.join().is_err() {
        std::process::exit(101);
    }
}

fn real_main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("render") => render(&args[1..]),
        Some("dom") => match args.get(1) {
            Some(u) => {
                let page = Page::load(&net::normalize(u), None);
                let mut s = String::new();
                page.dom.dump(0, &mut s);
                print!("{s}");
                Ok(())
            }
            None => Err(USAGE.to_string()),
        },
        other => open(other.unwrap_or("about:home")),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn render(args: &[String]) -> Result<(), String> {
    let mut url = None;
    let mut out = "out.png".to_string();
    let mut width = 1024.0f32;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => out = it.next().ok_or("-o needs a path")?.clone(),
            "-w" => {
                width = it.next().and_then(|w| w.parse().ok()).ok_or("-w needs a number")?;
                width = width.clamp(100.0, 8000.0);
            }
            u => url = Some(u.to_string()),
        }
    }
    let url = url.ok_or(USAGE)?;
    let page = load(&url);
    let r = page.render(width, 768.0);
    let height = (r.height.ceil() as usize).clamp(1, 20_000);
    let mut canvas = paint::Canvas::new(width as usize, height, r.background);
    canvas.paint(&r.list, 0.0, 0.0, &mut paint::GlyphCache::default());
    canvas.write_png(&out).map_err(|e| e.to_string())?;
    println!("{out}: {}x{height} \"{}\"", width as usize, page.title);
    Ok(())
}

fn load(input: &str) -> Page {
    if input == "about:home" {
        return Page::from_html("about:home", include_str!("../pages/home.html"));
    }
    Page::load(&net::normalize(input), None)
}

#[cfg(feature = "gui")]
fn open(input: &str) -> Result<(), String> {
    let start = if input == "about:home" {
        let p = std::env::temp_dir().join("brickle-home.html");
        std::fs::write(&p, include_str!("../pages/home.html")).map_err(|e| e.to_string())?;
        format!("file://{}", p.display())
    } else {
        net::normalize(input)
    };
    gui::run(&start)
}

#[cfg(not(feature = "gui"))]
fn open(_: &str) -> Result<(), String> {
    Err(format!("built without the `gui` feature\n\n{USAGE}"))
}
