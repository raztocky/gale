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
  brickle cookies                                   list sites whose cookies are kept, and saved cookies
  brickle cookies keep|unkeep SITE                  keep a site's cookies after closing, or stop
  brickle cookies forget SITE                       delete a site's saved cookies
  brickle js                                        list sites where JavaScript is on
  brickle js allow|block SITE                       turn JavaScript on or off for a site
  brickle sandbox-test                              check the renderer sandbox works here

OPTIONS:
  --no-sandbox   parse pages in the browser process instead of a sandboxed
                 renderer (for debugging; less safe)
  --no-filters   don't load filter lists or your rules (for debugging)
  --panel        open the window with the request panel showing
  --js           run JavaScript on every site, this time only (it's off
                 unless you allow a site; never with --no-sandbox)

Your rules live in ~/.config/brickle/rules.txt; filter lists in
~/.local/share/brickle/filters/. Cookies are deleted when Brickle closes,
except for sites in ~/.config/brickle/keep-cookies.txt. See PRIVACY.md for what Brickle will
and won't do on the network.
";

/// The built-in start page (`about:home`).
pub(crate) const HOME: &str = include_str!("../pages/home.html");

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

/// Hidden easter egg (not in --help): a red cap, worn backwards.
const NOOKIE: &str = r#"
                 _.-'''''-._
               .'  BRICKLE  '.
              /    _______    \
             |   .'       '.   |=================.
              \_/  (=^.^=)  \_/__________________/
                '-.._____..-'

   red cap on backwards. sandbox on forwards.
   0 trackers got through tonight. turn it up.
"#;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--nookie") {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            println!("\x1b[1;31m{NOOKIE}\x1b[0m");
        } else {
            println!("{NOOKIE}");
        }
        return;
    }

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
        // One probe per process: see sandbox::PROBES.
        let name = args.get(1).cloned().unwrap_or_default();
        let report = match sandbox::enter() {
            Ok(r) => r,
            Err(e) => {
                println!("FAILED to enter sandbox: {e}");
                std::process::exit(3);
            }
        };
        if name.is_empty() {
            println!(
                "namespaces: {}\nlandlock: {}\nseccomp: {}",
                report.namespaces,
                report.landlock,
                if report.seccomp { "on" } else { "off" }
            );
            std::process::exit(0);
        }
        // Run on a thread, like the real renderer, so thread creation under
        // the filter is exercised too.
        let blocked = std::thread::spawn(move || sandbox::probe(&name))
            .join()
            .unwrap_or(false);
        println!("{}", if blocked { "blocked" } else { "ALLOWED" });
        std::process::exit(0);
    }

    let sandboxed = !args.iter().any(|a| a == "--no-sandbox");
    let filters = !args.iter().any(|a| a == "--no-filters");
    let panel = args.iter().any(|a| a == "--panel");
    let js = args.iter().any(|a| a == "--js");
    args.retain(|a| !matches!(a.as_str(), "--no-sandbox" | "--no-filters" | "--panel" | "--js"));
    with_big_stack(move || match run(args, sandboxed, filters, panel, js) {
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

fn run(args: Vec<String>, sandboxed: bool, filters: bool, panel: bool, js: bool) -> Result<(), String> {
    let network = |filters: bool| {
        let mut n = network(filters);
        n.js_everywhere = js;
        n
    };
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
            if !tab.console.is_empty() {
                println!("console:");
                for l in &tab.console {
                    println!("  {l}");
                }
            }
            Ok(())
        }
        Some("cookies") => cookies_cmd(&args[1..]),
        Some("js") => js_cmd(&args[1..]),
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("render") => render(&args[1..], sandboxed, filters),
        Some("sandbox-test") => {
            // Each probe runs in a fresh sandboxed process; this one stays
            // unrestricted.
            use std::os::unix::process::ExitStatusExt;
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let run = |probe: &str| {
                std::process::Command::new(&exe)
                    .arg("--sandbox-probe")
                    .arg(probe)
                    .output()
            };
            let info = run("").map_err(|e| e.to_string())?;
            print!("{}", String::from_utf8_lossy(&info.stdout));
            if !info.status.success() {
                return Err("sandbox check FAILED: couldn't enter the sandbox".into());
            }
            let mut ok = true;
            for probe in sandbox::PROBES {
                let out = run(probe).map_err(|e| e.to_string())?;
                let stdout = String::from_utf8_lossy(&out.stdout);
                let verdict = if out.status.signal() == Some(31) {
                    "killed".to_string() // SIGSYS: seccomp killed it
                } else if stdout.trim() == "blocked" {
                    "blocked".to_string()
                } else {
                    ok = false;
                    format!("ALLOWED ({})", stdout.trim())
                };
                println!("  {verdict:<8} {probe}");
            }
            if ok {
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

fn js_cmd(args: &[String]) -> Result<(), String> {
    let mut n = Network::from_disk();
    match (args.first().map(String::as_str), args.get(1)) {
        (None | Some("list"), _) => {
            let sites = n.js_sites();
            if sites.is_empty() {
                println!("JavaScript is off everywhere. `brickle js allow SITE` turns it on for a site.");
            } else {
                println!("JavaScript is on for:");
                for s in sites {
                    println!("  {s}");
                }
            }
            Ok(())
        }
        (Some("allow"), Some(site)) => {
            println!("JavaScript on for {}", n.set_scripts_allowed(site, true)?);
            Ok(())
        }
        (Some("block"), Some(site)) => {
            println!("JavaScript off for {}", n.set_scripts_allowed(site, false)?);
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    }
}

fn cookies_cmd(args: &[String]) -> Result<(), String> {
    use brickle::network::{config_dir, data_dir};
    let mut jar = brickle::cookies::Jar::from_disk(config_dir().as_deref(), data_dir().as_deref());
    match (args.first().map(String::as_str), args.get(1)) {
        (None | Some("list"), _) => {
            let kept = jar.kept();
            if kept.is_empty() {
                println!("No sites on your keep list: all cookies are deleted when Brickle closes.");
            } else {
                println!("Cookies kept after closing Brickle for:");
                for s in kept {
                    let names: Vec<String> = jar.for_site(&s).iter().map(|c| c.name.clone()).collect();
                    println!("  {s:<30} {} saved {}", names.len(), names.join(", "));
                }
            }
            Ok(())
        }
        (Some("keep"), Some(site)) => {
            jar.set_kept(site, true)?;
            println!("keeping cookies for {}", brickle::cookies::site(site));
            Ok(())
        }
        (Some("unkeep"), Some(site)) => {
            jar.forget(site);
            jar.set_kept(site, false)?;
            println!(
                "{}: cookies will be deleted when Brickle closes",
                brickle::cookies::site(site)
            );
            Ok(())
        }
        (Some("forget"), Some(site)) => {
            let n = jar.forget(site);
            jar.save()?;
            println!("deleted {n} saved cookie(s) for {}", brickle::cookies::site(site));
            Ok(())
        }
        _ => Err(USAGE.to_string()),
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
        Outcome::OverLimit => "SKIPPED (page budget)",
        Outcome::LocalNetwork => "BLOCKED (local network)",
        Outcome::BadCertificate(err) => {
            notes.push(err.clone());
            "BLOCKED (bad certificate)"
        }
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
    if !e.cookies_sent.is_empty() {
        notes.push(format!("sent cookies {}", e.cookies_sent.join(", ")));
    }
    if !e.cookies_set.is_empty() {
        notes.push(format!("set cookies {}", e.cookies_set.join(", ")));
    }
    if e.cors_blocked {
        notes.push("response hidden from the page (CORS)".into());
    }
    if !e.cookies_refused.is_empty() {
        notes.push(format!("refused cookies {}", e.cookies_refused.join(", ")));
    }
    (status, notes.join("; "))
}

fn print_log(log: &[LogEntry]) {
    let kind = |e: &LogEntry| match e.dest {
        net::Dest::Document => "page",
        net::Dest::Stylesheet => "css",
        net::Dest::Script => "js",
        net::Dest::Xhr => "xhr",
        net::Dest::Image => "img",
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
    canvas.paint(&r.list, &r.atlas, 0.0, 0.0);
    canvas.write_png(&out).map_err(|e| e.to_string())?;
    println!("{out}: {}x{height} \"{}\"", width as usize, tab.title);
    Ok(())
}

#[cfg(feature = "gui")]
fn open_window(input: &str, sandboxed: bool, net: Network, panel: bool) -> Result<(), String> {
    let start = net::normalize(input);
    gui::run(&start, sandboxed, net, panel)
}

#[cfg(not(feature = "gui"))]
fn open_window(_: &str, _: bool, _: Network, _: bool) -> Result<(), String> {
    Err(format!("built without the `gui` feature\n\n{USAGE}"))
}
