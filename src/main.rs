#![forbid(unsafe_code)]

#[cfg(feature = "gui")]
mod gui;

use brickle::{net, paint, renderer, sandbox, tab::Tab};

const USAGE: &str = "\
brickle — Brickle Browser, a tiny from-scratch browser engine

USAGE:
  brickle [URL|FILE]                                open a browser window
  brickle render URL|FILE [-o out.png] [-w WIDTH]   render a full page to PNG
  brickle dom URL|FILE                              print the parsed DOM tree
  brickle sandbox-test                              check the renderer sandbox works here

OPTIONS:
  --no-sandbox   parse pages in the browser process instead of a sandboxed
                 renderer (for debugging; less safe)
";

const HOME: &str = include_str!("../pages/home.html");

/// Style, layout and paint recurse over the tree. The parser caps nesting
/// depth, and a roomy stack gives that cap a wide safety margin.
fn with_big_stack(f: impl FnOnce() -> i32 + Send + 'static) -> ! {
    let worker = std::thread::Builder::new()
        .name("brickle".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .expect("failed to start main thread");
    std::process::exit(worker.join().unwrap_or(101))
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(String::as_str) == Some("--renderer") {
        // Lock the process down *before* touching any untrusted input.
        if let Err(e) = sandbox::enter() {
            eprintln!("brickle renderer: can't enter sandbox: {e}");
            std::process::exit(3);
        }
        with_big_stack(|| match renderer::run() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("brickle renderer: {e}");
                1
            }
        });
    }

    if args.first().map(String::as_str) == Some("--sandbox-probe") {
        let report = match sandbox::enter() {
            Ok(r) => r,
            Err(e) => {
                println!("FAILED to enter sandbox: {e}");
                std::process::exit(3);
            }
        };
        println!(
            "landlock: {}\nseccomp: {}",
            report.landlock,
            if report.seccomp { "on" } else { "off" }
        );
        let mut all = true;
        for (what, blocked) in sandbox::probe() {
            all &= blocked;
            println!("  {} {what}", if blocked { "blocked" } else { "ALLOWED" });
        }
        std::process::exit(if all { 0 } else { 2 });
    }

    let sandboxed = !args.iter().any(|a| a == "--no-sandbox");
    args.retain(|a| a != "--no-sandbox");
    with_big_stack(move || match run(args, sandboxed) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}

fn run(args: Vec<String>, sandboxed: bool) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("render") => render(&args[1..], sandboxed),
        Some("sandbox-test") => {
            // Run the probe in a fresh process so this one stays unrestricted.
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let status = std::process::Command::new(exe)
                .arg("--sandbox-probe")
                .status()
                .map_err(|e| e.to_string())?;
            if status.success() {
                println!("sandbox OK: the renderer can't touch files, the network or other programs");
                Ok(())
            } else {
                Err("sandbox check FAILED".into())
            }
        }
        Some("dom") => {
            let u = args.get(1).ok_or(USAGE)?;
            print!("{}", open_tab(u, sandboxed).dom()?);
            Ok(())
        }
        other => open_window(other.unwrap_or("about:home"), sandboxed),
    }
}

fn open_tab(input: &str, sandboxed: bool) -> Tab {
    if input == "about:home" {
        return Tab::with_html("about:home", None, HOME, sandboxed);
    }
    Tab::open(&net::normalize(input), None, sandboxed)
}

fn render(args: &[String], sandboxed: bool) -> Result<(), String> {
    let mut url = None;
    let mut out = "out.png".to_string();
    let mut width = 1024.0f32;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => out = it.next().ok_or("-o needs a path")?.clone(),
            "-w" => {
                width = it
                    .next()
                    .and_then(|w| w.parse().ok())
                    .ok_or("-w needs a number")?;
                width = width.clamp(100.0, 8000.0);
            }
            u => url = Some(u.to_string()),
        }
    }
    let mut tab = open_tab(&url.ok_or(USAGE)?, sandboxed);
    let r = tab.render(width, 768.0);
    let height = (r.height.ceil() as usize).clamp(1, 20_000);
    let mut canvas = paint::Canvas::new(width as usize, height, r.background);
    canvas.paint(&r.list, 0.0, 0.0, &mut paint::GlyphCache::default());
    canvas.write_png(&out).map_err(|e| e.to_string())?;
    println!("{out}: {}x{height} \"{}\"", width as usize, tab.title);
    Ok(())
}

#[cfg(feature = "gui")]
fn open_window(input: &str, sandboxed: bool) -> Result<(), String> {
    let start = if input == "about:home" {
        let p = std::env::temp_dir().join("brickle-home.html");
        std::fs::write(&p, HOME).map_err(|e| e.to_string())?;
        format!("file://{}", p.display())
    } else {
        net::normalize(input)
    };
    gui::run(&start, sandboxed)
}

#[cfg(not(feature = "gui"))]
fn open_window(_: &str, _: bool) -> Result<(), String> {
    Err(format!("built without the `gui` feature\n\n{USAGE}"))
}
