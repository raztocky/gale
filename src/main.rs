#![forbid(unsafe_code)]

#[cfg(feature = "gui")]
mod gui;

use brickle::network::{LogEntry, Network, Outcome};
use brickle::{lists, net, paint, renderer, sandbox, tab::Tab};

const USAGE: &str = "\
brickle — Brickle Browser, a tiny from-scratch browser engine

USAGE:
  brickle [URL|FILE]                                open a browser window
  brickle render URL|FILE [-o out.png] [-w WIDTH]   render a full page to PNG
  brickle dom URL|FILE                              print the parsed DOM tree
  brickle requests URL|FILE                         list every request a page makes
  brickle filters update                            download the filter lists (prints each request)
  brickle filters status                            show installed filter lists
  brickle sandbox-test                              check the renderer sandbox works here

OPTIONS:
  --no-sandbox   parse pages in the browser process instead of a sandboxed
                 renderer (for debugging; less safe)
  --no-filters   don't load filter lists or your rules (for debugging)
  --panel        open the window with the request panel showing

Your rules live in ~/.config/brickle/rules.txt; filter lists in
~/.local/share/brickle/filters/. See PRIVACY.md for what Brickle will
and won't do on the network.
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
    let filters = !args.iter().any(|a| a == "--no-filters");
    let panel = args.iter().any(|a| a == "--panel");
    args.retain(|a| a != "--no-sandbox" && a != "--no-filters" && a != "--panel");
    with_big_stack(move || match run(args, sandboxed, filters, panel) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}

fn network(filters: bool) -> Network {
    if !filters {
        return Network::empty();
    }
    let n = Network::from_disk();
    if n.lists_loaded == 0 {
        eprintln!("brickle: no filter lists installed; run `brickle filters update` to download them");
    }
    n
}

fn run(args: Vec<String>, sandboxed: bool, filters: bool, panel: bool) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("filters") => match args.get(1).map(String::as_str) {
            Some("update") => {
                println!("Downloading filter lists. Every request is listed:");
                lists::update(&mut |l| println!("{l}"))
            }
            Some("status") | None => {
                for l in lists::status() {
                    println!("{l}");
                }
                Ok(())
            }
            _ => Err(USAGE.to_string()),
        },
        Some("requests") => {
            let u = args.get(1).ok_or(USAGE)?;
            let mut net = network(filters);
            let mut tab = open_tab(&mut net, u, sandboxed);
            let _ = tab.render(1024.0, 768.0);
            print_log(&tab.log);
            Ok(())
        }
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("render") => render(&args[1..], sandboxed, filters),
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
            print!("{}", open_tab(&mut network(filters), u, sandboxed).dom()?);
            Ok(())
        }
        other => open_window(other.unwrap_or("about:home"), sandboxed, network(filters), panel),
    }
}

fn open_tab(net: &mut Network, input: &str, sandboxed: bool) -> Tab {
    if input == "about:home" {
        return Tab::with_html(net, "about:home", None, HOME, sandboxed);
    }
    Tab::open(net, &net::normalize(input), None, sandboxed)
}

/// One line per request, as in the window's request panel.
pub fn describe(e: &LogEntry) -> (&'static str, String) {
    let mut notes = Vec::new();
    let status = match &e.outcome {
        Outcome::Loaded { status } => {
            notes.push(format!("HTTP {status}"));
            "sent"
        }
        Outcome::Local => "local",
        Outcome::Redirected { to } => {
            notes.push(format!("redirect to {to}"));
            "sent"
        }
        Outcome::Blocked { rule } => {
            notes.push(format!("filter rule {rule}"));
            "BLOCKED"
        }
        Outcome::Denied { rule } => {
            notes.push(format!("your rule {rule}"));
            "DENIED"
        }
        Outcome::MixedContent => "BLOCKED (http on https page)",
        Outcome::NeedsHttpConsent => "no HTTPS",
        Outcome::NotAllowed => "BLOCKED (not allowed)",
        Outcome::OverLimit => "SKIPPED (too many)",
        Outcome::Failed(err) => {
            notes.push(err.clone());
            "failed"
        }
    };
    if e.upgraded {
        notes.push("upgraded to https".into());
    }
    if e.insecure {
        notes.push("INSECURE http".into());
    }
    if !e.stripped.is_empty() {
        notes.push(format!("stripped {}", e.stripped.join(", ")));
    }
    if let Some(r) = &e.allowed_by {
        notes.push(format!("allowed by {r}"));
    }
    (status, notes.join("; "))
}

fn print_log(log: &[LogEntry]) {
    let kind = |e: &LogEntry| match e.dest {
        net::Dest::Document => "page",
        net::Dest::Stylesheet => "css",
        net::Dest::Download => "list",
    };
    for e in log {
        let (status, notes) = describe(e);
        println!(
            "{:<8} {:<5} {}{}",
            status,
            kind(e),
            e.url,
            if notes.is_empty() {
                String::new()
            } else {
                format!("\n               {notes}")
            }
        );
    }
    let sent = log.iter().filter(|e| e.was_sent()).count();
    println!(
        "{} requests: {} sent, {} blocked or denied",
        log.len(),
        sent,
        log.len() - sent
    );
}

fn render(args: &[String], sandboxed: bool, filters: bool) -> Result<(), String> {
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
    let mut tab = open_tab(&mut network(filters), &url.ok_or(USAGE)?, sandboxed);
    let r = tab.render(width, 768.0);
    let height = (r.height.ceil() as usize).clamp(1, 20_000);
    let mut canvas = paint::Canvas::new(width as usize, height, r.background);
    canvas.paint(&r.list, 0.0, 0.0, &mut paint::GlyphCache::default());
    canvas.write_png(&out).map_err(|e| e.to_string())?;
    println!("{out}: {}x{height} \"{}\"", width as usize, tab.title);
    Ok(())
}

#[cfg(feature = "gui")]
fn open_window(input: &str, sandboxed: bool, net: Network, panel: bool) -> Result<(), String> {
    let start = if input == "about:home" {
        let p = std::env::temp_dir().join("brickle-home.html");
        std::fs::write(&p, HOME).map_err(|e| e.to_string())?;
        format!("file://{}", p.display())
    } else {
        net::normalize(input)
    };
    gui::run(&start, sandboxed, net, panel)
}

#[cfg(not(feature = "gui"))]
fn open_window(_: &str, _: bool, _: Network, _: bool) -> Result<(), String> {
    Err(format!("built without the `gui` feature\n\n{USAGE}"))
}
