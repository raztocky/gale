//! Low-level loading: local files and single HTTP(S) exchanges.
//!
//! Policy (blocking, HTTPS-only, redirects, logging) lives in
//! [`crate::network`]. This module is the only place in Brickle that opens
//! network connections; a test enforces that.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest a single request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Requests that may be running at once, counting ones abandoned by Stop
/// that are still waiting to connect (that phase can't be interrupted).
const MAX_IN_FLIGHT: usize = 8;
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Counts a running request; uncounted when dropped.
struct InFlight;

impl InFlight {
    fn start() -> Option<InFlight> {
        // Take a slot, and give it back if there wasn't one.
        if IN_FLIGHT.fetch_add(1, Ordering::SeqCst) < MAX_IN_FLIGHT {
            Some(InFlight)
        } else {
            IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
            None
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Stops reading a response (and so closes its connection) once Stop is
/// pressed or the deadline passes.
struct Stoppable<R> {
    inner: R,
    cancel: Cancel,
    deadline: Instant,
}

impl<R: Read> Read for Stoppable<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(std::io::Error::other("stopped"));
        }
        if Instant::now() >= self.deadline {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"));
        }
        self.inner.read(buf)
    }
}

/// Why a request failed. Kept typed so policy can tell, for example, a bad
/// certificate (never a reason to fall back to HTTP) from a site that just
/// doesn't speak HTTPS.
#[derive(Debug, Clone, PartialEq)]
pub enum NetError {
    /// A public website reaching for a local-network address.
    LocalNetwork,
    /// The server's TLS certificate couldn't be verified.
    Certificate(String),
    /// You pressed Stop.
    Cancelled,
    /// Ran past the request or page deadline.
    TimedOut,
    Other(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            NetError::LocalNetwork => f.write_str(LOCAL_NETWORK_ERR),
            NetError::Certificate(e) => write!(f, "certificate error: {e}"),
            NetError::Cancelled => f.write_str("stopped"),
            NetError::TimedOut => f.write_str("timed out"),
            NetError::Other(e) => f.write_str(e),
        }
    }
}

/// A Stop button: shared between the window and whatever is loading.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Largest document or stylesheet we will read.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Brickle presents itself as stock Firefox on Linux, sending the same
/// headers in the same order, so it doesn't stand out. Bump this with
/// Firefox releases (see `browser/config/version_display.txt` in Firefox).
pub const FIREFOX_VERSION: &str = "159.0";

pub fn user_agent() -> String {
    format!(
        "Mozilla/5.0 (X11; Linux x86_64; rv:{v}) Gecko/20100101 Firefox/{v}",
        v = FIREFOX_VERSION
    )
}

static REQUESTS: AtomicUsize = AtomicUsize::new(0);

/// How many HTTP requests this process has sent (for privacy tests).
pub fn requests_sent() -> usize {
    REQUESTS.load(Ordering::SeqCst)
}

/// The scheme of a URL, as the URL parser sees it ("" if it doesn't parse).
pub fn scheme(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.scheme().to_string())
        .unwrap_or_default()
}

/// The one way Brickle turns a string into a URL for security decisions:
/// the WHATWG parser from the `url` crate. Everything downstream (filters,
/// cookies, HTTPS-only, the local-network check) sees its serialization, so
/// no two parts of the browser can disagree about which host a URL means.
/// Credentials (`user:pass@`) are removed: they are a classic way to
/// disguise a link's real host, and Brickle never sends them.
pub fn canonical(url: &str) -> Option<(url::Url, bool)> {
    let mut u = url::Url::parse(url.trim()).ok()?;
    let had_credentials = !u.username().is_empty() || u.password().is_some();
    if had_credentials {
        u.set_username("").ok()?;
        u.set_password(None).ok()?;
    }
    Some((u, had_credentials))
}

/// May a document at `from` load `to`? `from` is `None` for user-initiated
/// loads (typed address, command line), which may open local files.
pub fn may_load(from: Option<&str>, to: &str) -> bool {
    match scheme(to).as_str() {
        "http" | "https" => true,
        "file" => from.is_none_or(|f| scheme(f) == "file"),
        _ => false,
    }
}

pub fn read_file(path: &str) -> Result<String, String> {
    read_file_bytes(path).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// A local file's bytes, with the same checks as [`read_file`].
pub fn read_file_bytes(path: &str) -> Result<Vec<u8>, String> {
    let path = url::Url::parse(path)
        .ok()
        .filter(|u| u.scheme() == "file")
        .and_then(|u| u.to_file_path().ok())
        .ok_or_else(|| format!("{path}: not a local file address"))?;
    let shown = path.display();
    let meta = std::fs::metadata(&path).map_err(|e| format!("{shown}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{shown}: not a regular file"));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!(
            "{shown}: file is larger than {} MB",
            MAX_BYTES / 1024 / 1024
        ));
    }
    std::fs::read(&path).map_err(|e| format!("{shown}: {e}"))
}

/// Loopback, private, link-local and similar non-public addresses.
pub fn is_local_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // carrier-grade NAT 100.64/10
                || o[0] == 0
        }
        std::net::IpAddr::V6(v6) => {
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80 // link local
                || v6.to_ipv4_mapped().is_some_and(|v4| is_local_ip(v4.into()))
        }
    }
}

/// Loopback and private-network hosts, judged by name or literal address
/// (no DNS lookup). HTTPS-only mode doesn't upgrade these (as in Firefox).
pub fn is_local_host(host: &str) -> bool {
    let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") {
        return true;
    }
    h.parse::<std::net::IpAddr>().is_ok_and(is_local_ip)
}

/// Prefix of the error returned when a public page's request resolves to a
/// local-network address.
pub const LOCAL_NETWORK_ERR: &str = "blocked: resolves to a local network address";

/// DNS resolver that drops local-network addresses. ureq connects to
/// exactly the addresses it returns, so a site can't pass this check and
/// then re-point its DNS at your router (DNS rebinding).
struct PublicOnly;

impl ureq::Resolver for PublicOnly {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        use std::net::ToSocketAddrs;
        let addrs: Vec<_> = netloc
            .to_socket_addrs()?
            .filter(|a| !is_local_ip(a.ip()))
            .collect();
        if addrs.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                LOCAL_NETWORK_ERR,
            ));
        }
        Ok(addrs)
    }
}

/// What kind of request, which decides the headers Firefox would send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    Document,
    Stylesheet,
    /// A page's script (only for sites where you allowed JavaScript).
    Script,
    /// A request a page's script made (`fetch`, `XMLHttpRequest`).
    Xhr,
    /// An `<img>`.
    Image,
    /// `brickle filters update`.
    Download,
}

/// Value for `Sec-Fetch-Site` given the requesting page (None = typed).
pub fn fetch_site(initiator: Option<&str>, target: &str) -> &'static str {
    let Some(from) = initiator else { return "none" };
    let (Ok(a), Ok(b)) = (url::Url::parse(from), url::Url::parse(target)) else {
        return "cross-site";
    };
    if a.origin() == b.origin() {
        "same-origin"
    } else if a.scheme() == b.scheme()
        && psl::domain_str(a.host_str().unwrap_or("")) == psl::domain_str(b.host_str().unwrap_or(""))
    {
        "same-site"
    } else {
        "cross-site"
    }
}

fn with_cookie(req: ureq::Request, cookie: Option<&str>) -> ureq::Request {
    match cookie {
        Some(c) => req.set("Cookie", c),
        None => req,
    }
}

/// A request made by a page's script (`fetch`/`XMLHttpRequest`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptRequest {
    /// Upper-case method.
    pub method: String,
    /// Headers the script set (already filtered by policy).
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub credentials: Credentials,
}

/// Whether a script request carries cookies (`fetch`'s `credentials`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Credentials {
    Omit,
    #[default]
    SameOrigin,
    Include,
}

/// Wire details of a script request for one hop.
#[derive(Debug, Clone, Copy)]
pub struct Outgoing<'a> {
    pub method: &'a str,
    pub headers: &'a [(String, String)],
    pub body: Option<&'a str>,
    /// `Origin` header value, if one is sent.
    pub origin: Option<&'a str>,
    /// A cross-origin (CORS) request rather than a same-origin one.
    pub cors: bool,
}

pub struct Response {
    pub status: u16,
    /// `Location` header of a redirect, unresolved.
    pub location: Option<String>,
    /// `Set-Cookie` headers, for the cookie jar.
    pub set_cookies: Vec<String>,
    /// Every response header (lower-case names), for script requests.
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// The raw body of an image (`body` is empty then).
    pub bytes: Vec<u8>,
}

/// One HTTP GET. Redirects are returned, not followed, so the caller can
/// check every hop. HTTP error statuses are returned with their body.
///
/// With `public_only`, the request is refused if the host is (or resolves
/// to) a local-network address: a website must not be able to reach your
/// router, printer or other devices through your browser.
pub fn http_get(
    url: &str,
    dest: Dest,
    initiator: Option<&str>,
    public_only: bool,
    cookie: Option<&str>,
) -> Result<Response, NetError> {
    http_get_timeout(
        None,
        url,
        dest,
        initiator,
        public_only,
        cookie,
        REQUEST_TIMEOUT,
        &Cancel::new(),
    )
}

/// [`http_get`] that gives up at `deadline` or when `cancel` is pressed,
/// whichever comes first. The request runs on its own thread, which is
/// abandoned (and ends at its own timeout) if we stop waiting.
#[allow(clippy::too_many_arguments)]
pub fn http_get_until(
    url: &str,
    dest: Dest,
    initiator: Option<&str>,
    public_only: bool,
    cookie: Option<&str>,
    deadline: Instant,
    cancel: &Cancel,
) -> Result<Response, NetError> {
    http_request_until(None, url, dest, initiator, public_only, cookie, deadline, cancel)
}

/// An owned copy of [`Outgoing`] for the request thread.
struct OwnedOutgoing {
    method: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    origin: Option<String>,
    cors: bool,
}

impl OwnedOutgoing {
    fn as_outgoing(&self) -> Outgoing<'_> {
        Outgoing {
            method: &self.method,
            headers: &self.headers,
            body: self.body.as_deref(),
            origin: self.origin.as_deref(),
            cors: self.cors,
        }
    }
}

/// [`http_get_until`] for any request, including a script's (`script`).
#[allow(clippy::too_many_arguments)]
pub fn http_request_until(
    script: Option<Outgoing>,
    url: &str,
    dest: Dest,
    initiator: Option<&str>,
    public_only: bool,
    cookie: Option<&str>,
    deadline: Instant,
    cancel: &Cancel,
) -> Result<Response, NetError> {
    let outgoing = script.map(|o| OwnedOutgoing {
        method: o.method.to_string(),
        headers: o.headers.to_vec(),
        body: o.body.map(str::to_string),
        origin: o.origin.map(str::to_string),
        cors: o.cors,
    });
    let left = deadline.saturating_duration_since(Instant::now());
    if cancel.is_cancelled() {
        return Err(NetError::Cancelled);
    }
    if left.is_zero() {
        return Err(NetError::TimedOut);
    }
    let Some(slot) = InFlight::start() else {
        return Err(NetError::Other(
            "too many requests still running (stopped ones are still timing out)".into(),
        ));
    };
    let worker_cancel = cancel.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let (url, initiator, cookie) = (
        url.to_string(),
        initiator.map(str::to_string),
        cookie.map(str::to_string),
    );
    std::thread::Builder::new()
        .name("brickle-http".into())
        .spawn(move || {
            let _slot = slot;
            let r = http_get_timeout(
                outgoing.as_ref().map(|o| o.as_outgoing()),
                &url,
                dest,
                initiator.as_deref(),
                public_only,
                cookie.as_deref(),
                left.min(REQUEST_TIMEOUT),
                &worker_cancel,
            );
            let _ = tx.send(r);
        })
        .map_err(|e| NetError::Other(format!("can't start request: {e}")))?;
    loop {
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(r) => return r,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(NetError::Other("request thread failed".into()))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel.is_cancelled() {
                    return Err(NetError::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Err(NetError::TimedOut);
                }
            }
        }
    }
}

/// Find a certificate verification failure anywhere in an error chain.
fn certificate_error(e: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut cur = Some(e);
    while let Some(err) = cur {
        if let Some(r) = err.downcast_ref::<rustls::Error>() {
            if matches!(
                r,
                rustls::Error::InvalidCertificate(_)
                    | rustls::Error::NoCertificatesPresented
                    | rustls::Error::InvalidCertRevocationList(_)
            ) {
                return Some(r.to_string());
            }
        }
        if let Some(inner) = err.downcast_ref::<std::io::Error>().and_then(|io| io.get_ref()) {
            if let Some(found) = certificate_error(inner) {
                return Some(found);
            }
        }
        cur = err.source();
    }
    None
}

fn timed_out(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur = Some(e);
    while let Some(err) = cur {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                return true;
            }
        }
        cur = err.source();
    }
    false
}

fn classify(e: &ureq::Error) -> NetError {
    let msg = e.to_string();
    if msg.contains(LOCAL_NETWORK_ERR) {
        NetError::LocalNetwork
    } else if let Some(c) = certificate_error(e) {
        NetError::Certificate(c)
    } else if timed_out(e) {
        NetError::TimedOut
    } else {
        NetError::Other(msg)
    }
}

#[allow(clippy::too_many_arguments)]
fn http_get_timeout(
    script: Option<Outgoing>,
    url: &str,
    dest: Dest,
    initiator: Option<&str>,
    public_only: bool,
    cookie: Option<&str>,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<Response, NetError> {
    let deadline = Instant::now() + timeout;
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    if public_only && is_local_host(&host) {
        return Err(NetError::LocalNetwork);
    }
    let proxied = proxy_configured() && !bypass_proxy(&host);
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(timeout.min(Duration::from_secs(10)))
        .timeout(timeout)
        .redirects(0)
        .try_proxy_from_env(!bypass_proxy(&host));
    // Through a proxy, the proxy resolves the name (and our resolver would
    // only see the proxy's own, often local, address), so only the literal
    // check above applies.
    if public_only && !proxied {
        builder = builder.resolver(PublicOnly);
    }
    let agent = builder.build();
    let site = fetch_site(initiator, url);
    // Header names, values and order as sent by Firefox.
    let method = script.map(|o| o.method).unwrap_or("GET");
    let mut req = agent.request(method, url).set("User-Agent", &user_agent());
    req = match dest {
        Dest::Document => {
            req = req
                .set(
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                )
                .set("Accept-Language", "en-US,en;q=0.5")
                .set("Accept-Encoding", "gzip, deflate, br, zstd");
            // A form POST: its content type and origin, as Firefox sends.
            if let Some(o) = script {
                if let Some((_, v)) = o
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    req = req.set("Content-Type", v);
                }
                if let Some(origin) = o.origin {
                    req = req.set("Origin", origin);
                }
            }
            req = req.set("Connection", "keep-alive");
            req = with_cookie(req, cookie);
            req.set("Upgrade-Insecure-Requests", "1")
                .set("Sec-Fetch-Dest", "document")
                .set("Sec-Fetch-Mode", "navigate")
                .set("Sec-Fetch-Site", site)
                .set("Sec-Fetch-User", "?1")
                .set("Priority", "u=0, i")
        }
        Dest::Stylesheet => {
            req = req
                .set("Accept", "text/css,*/*;q=0.1")
                .set("Accept-Language", "en-US,en;q=0.5")
                .set("Accept-Encoding", "gzip, deflate, br, zstd")
                .set("Connection", "keep-alive");
            req = with_cookie(req, cookie);
            req.set("Sec-Fetch-Dest", "style")
                .set("Sec-Fetch-Mode", "no-cors")
                .set("Sec-Fetch-Site", site)
                .set("Priority", "u=2")
        }
        Dest::Script => {
            req = req
                .set("Accept", "*/*")
                .set("Accept-Language", "en-US,en;q=0.5")
                .set("Accept-Encoding", "gzip, deflate, br, zstd")
                .set("Connection", "keep-alive");
            req = with_cookie(req, cookie);
            req.set("Sec-Fetch-Dest", "script")
                .set("Sec-Fetch-Mode", "no-cors")
                .set("Sec-Fetch-Site", site)
        }
        Dest::Xhr => {
            let o = script.ok_or_else(|| NetError::Other("script request without details".into()))?;
            let accept = o
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("accept"))
                .map(|(_, v)| v.as_str())
                .unwrap_or("*/*");
            req = req
                .set("Accept", accept)
                .set("Accept-Language", "en-US,en;q=0.5")
                .set("Accept-Encoding", "gzip, deflate, br, zstd");
            for (k, v) in o
                .headers
                .iter()
                .filter(|(k, _)| !k.eq_ignore_ascii_case("accept"))
            {
                req = req.set(k, v);
            }
            if let Some(origin) = o.origin {
                req = req.set("Origin", origin);
            }
            req = req.set("Connection", "keep-alive");
            req = with_cookie(req, cookie);
            req.set("Sec-Fetch-Dest", "empty")
                .set("Sec-Fetch-Mode", if o.cors { "cors" } else { "same-origin" })
                .set("Sec-Fetch-Site", site)
                .set("Priority", "u=4")
        }
        Dest::Image => {
            req = req
                .set(
                    "Accept",
                    "image/avif,image/webp,image/png,image/svg+xml,image/*;q=0.8,*/*;q=0.5",
                )
                .set("Accept-Language", "en-US,en;q=0.5")
                .set("Accept-Encoding", "gzip, deflate, br, zstd")
                .set("Connection", "keep-alive");
            req = with_cookie(req, cookie);
            req.set("Sec-Fetch-Dest", "image")
                .set("Sec-Fetch-Mode", "no-cors")
                .set("Sec-Fetch-Site", site)
                .set("Priority", "u=5, i")
        }
        Dest::Download => req
            .set("Accept", "*/*")
            .set("Accept-Language", "en-US,en;q=0.5")
            .set("Accept-Encoding", "gzip, deflate, br, zstd")
            .set("Connection", "keep-alive"),
    };
    REQUESTS.fetch_add(1, Ordering::SeqCst);
    let sent = match script.and_then(|o| o.body) {
        Some(body) => req.send_string(body),
        None => req.call(),
    };
    let resp = match sent {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(classify(&e)),
    };
    let status = resp.status();
    let set_cookies: Vec<String> = resp.all("set-cookie").into_iter().map(str::to_string).collect();
    let mut headers = Vec::new();
    for name in resp.headers_names() {
        for v in resp.all(&name) {
            headers.push((name.to_ascii_lowercase(), v.to_string()));
        }
    }
    if (300..400).contains(&status) {
        return Ok(Response {
            status,
            location: resp.header("location").map(str::to_string),
            set_cookies,
            headers,
            body: String::new(),
            bytes: vec![],
        });
    }
    let ctype = resp.content_type().to_ascii_lowercase();
    let texty = ctype.starts_with("text/")
        || ctype.contains("html")
        || ctype.contains("xml")
        || ctype.contains("css")
        || ctype.contains("javascript")
        || ctype.contains("ecmascript")
        || ctype.contains("json")
        || ctype.is_empty();
    // Images: any image type but SVG (which is a document, with scripts).
    // What it really is gets decided from its bytes, in the renderer.
    let image = dest == Dest::Image
        && (ctype.is_empty()
            || (ctype.starts_with("image/") && !ctype.contains("svg"))
            || ctype == "application/octet-stream");
    if dest == Dest::Image && !image {
        return Err(NetError::Other(format!("not an image ({ctype})")));
    }
    if !texty && !image {
        return Err(NetError::Other(format!("can't display content of type {ctype}")));
    }
    // ureq decodes gzip and brotli itself; deflate and zstd are done here.
    let encoding = resp
        .header("content-encoding")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let reader: Box<dyn Read> = Box::new(Stoppable {
        inner: resp.into_reader().take(MAX_BYTES * 4),
        cancel: cancel.clone(),
        deadline,
    });
    let mut decoded: Box<dyn Read> = match encoding.as_str() {
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(reader)),
        "zstd" => Box::new(
            ruzstd::decoding::StreamingDecoder::new(reader)
                .map_err(|e| NetError::Other(format!("zstd: {e}")))?,
        ),
        _ => reader,
    };
    let mut bytes = Vec::new();
    // Bounded after decompression too, so a compression bomb can't blow up.
    (&mut decoded)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| {
            if cancel.is_cancelled() {
                NetError::Cancelled
            } else if matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                NetError::TimedOut
            } else {
                NetError::Other(e.to_string())
            }
        })?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(NetError::Other(format!(
            "response is larger than {} MB",
            MAX_BYTES / 1024 / 1024
        )));
    }
    Ok(Response {
        status,
        location: None,
        set_cookies,
        headers,
        body: if image {
            String::new()
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        },
        bytes: if image { bytes } else { vec![] },
    })
}

fn proxy_configured() -> bool {
    [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .any(|v| std::env::var(v).is_ok_and(|x| !x.trim().is_empty()))
}

/// Honour NO_PROXY (ureq 2 doesn't): comma-separated hosts, `.suffix` or
/// `*.suffix` domains, and `*` for everything.
fn bypass_proxy(host: &str) -> bool {
    let list = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    no_proxy_matches(&list, host)
}

fn no_proxy_matches(list: &str, host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    list.split(',').map(|e| e.trim().to_ascii_lowercase()).any(|e| {
        if e.is_empty() {
            return false;
        }
        if e == "*" {
            return true;
        }
        let e = e.trim_start_matches('*');
        if let Some(suffix) = e.strip_prefix('.') {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == e
        }
    })
}

/// The URL as it should be shown in the address bar: the host is converted to
/// its ASCII (punycode) form and any `user:pass@` part is dropped, so a
/// lookalike or `trusted.com@evil.com` URL can't impersonate another site.
/// Returns (prefix up to and including the host, rest).
pub fn display_parts(input: &str) -> (String, String) {
    match url::Url::parse(input) {
        Ok(u) if u.host_str().is_some() => {
            let mut head = format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""));
            if let Some(port) = u.port() {
                head.push_str(&format!(":{port}"));
            }
            let mut rest = u.path().to_string();
            if rest == "/" {
                rest.clear();
            }
            if let Some(q) = u.query() {
                rest.push('?');
                rest.push_str(q);
            }
            if let Some(f) = u.fragment() {
                rest.push('#');
                rest.push_str(f);
            }
            (head, rest)
        }
        _ => (String::new(), input.to_string()),
    }
}

/// Turn a user-typed address or a path into an absolute URL.
pub fn normalize(input: &str) -> String {
    let input = input.trim();
    if input.starts_with("about:") {
        return input.to_string();
    }
    if input.contains("://") {
        return url::Url::parse(input)
            .map(String::from)
            .unwrap_or_else(|_| input.to_string());
    }
    if let Ok(p) = std::fs::canonicalize(input) {
        if let Ok(u) = url::Url::from_file_path(&p) {
            return u.into();
        }
    }
    url::Url::parse(&format!("https://{input}"))
        .map(String::from)
        .unwrap_or_else(|_| format!("https://{input}"))
}

/// Resolve `href` relative to `base`, with the URL parser. Anything that
/// doesn't resolve becomes `about:invalid`, which nothing will load.
pub fn resolve(base: &str, href: &str) -> String {
    url::Url::parse(base)
        .and_then(|b| b.join(href.trim()))
        .or_else(|_| url::Url::parse(href.trim()))
        .map(String::from)
        .unwrap_or_else(|_| "about:invalid".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves() {
        assert_eq!(
            resolve("https://a.com/x/y.html", "z.html"),
            "https://a.com/x/z.html"
        );
        assert_eq!(resolve("https://a.com/x/y.html", "../z"), "https://a.com/z");
        assert_eq!(resolve("https://a.com/x/", "/q?a=1"), "https://a.com/q?a=1");
        assert_eq!(resolve("https://a.com", "b"), "https://a.com/b");
        assert_eq!(resolve("file:///tmp/a/b.html", "c.css"), "file:///tmp/a/c.css");
        assert_eq!(resolve("https://a.com/p", "//cdn.x/y"), "https://cdn.x/y");
        assert_eq!(resolve("https://a.com/p#x", "#y"), "https://a.com/p#y");
        assert_eq!(resolve("https://a.com/p", "brickle:proceed"), "brickle:proceed");
    }

    /// Tricky URLs: whatever host the URL parser sees is the host every
    /// security check sees.
    #[test]
    fn one_parser_for_tricky_urls() {
        let host = |u: &str| crate::filter::host_of(&resolve("https://page.example/dir/", u));
        // Backslashes are slashes in web URLs.
        assert_eq!(host("https://evil.example\\@good.example/"), "evil.example");
        assert_eq!(host("\\\\evil.example/x"), "evil.example");
        // Encoded dot segments can't climb out of the path or change host.
        assert_eq!(
            resolve("https://a.example/x/y/", "%2e%2e/%2E%2e/z"),
            "https://a.example/z"
        );
        // Credentials don't hide the real host, and are removed.
        assert_eq!(host("https://good.example@evil.example/"), "evil.example");
        let (u, had) = canonical("https://user:pw@evil.example/x").unwrap();
        assert!(had);
        assert_eq!(u.as_str(), "https://evil.example/x");
        // Unusual IPv4 notation is normalized before the local-network check.
        for local in [
            "http://0x7f.1/",
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://192.168.1/",
        ] {
            let h = host(local);
            assert!(is_local_host(&h), "{local} -> {h}");
        }
        // Case, whitespace and tabs are normalized.
        assert_eq!(host("  HTTPS://EVIL.EXAMPLE/  "), "evil.example");
        assert_eq!(host("https://ev\til.example/"), "evil.example");
        // Schemes are what the parser says.
        assert_eq!(scheme("FILE:///etc/passwd"), "file");
        assert!(!may_load(
            Some("https://a.example/"),
            &resolve("https://a.example/", "FiLe:///etc/passwd")
        ));
        assert!(!may_load(
            Some("https://a.example/"),
            &resolve("https://a.example/", " javascript:alert(1)")
        ));
        assert_eq!(resolve("https://a.example/", "http://[::1"), "about:invalid");
    }

    #[test]
    fn stop_and_deadline_interrupt_a_stalled_request() {
        // A server that accepts connections and never answers.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/", l.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let held: Vec<_> = l.incoming().take(4).collect();
            std::thread::sleep(Duration::from_secs(30));
            drop(held);
        });
        let cancel = Cancel::new();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.cancel();
        });
        let far = Instant::now() + Duration::from_secs(30);
        let start = Instant::now();
        let r = http_get_until(&url, Dest::Stylesheet, None, false, None, far, &cancel);
        assert_eq!(r.err(), Some(NetError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(2));

        let start = Instant::now();
        let soon = Instant::now() + Duration::from_millis(300);
        let r = http_get_until(&url, Dest::Stylesheet, None, false, None, soon, &Cancel::new());
        assert_eq!(r.err(), Some(NetError::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn stop_closes_a_slow_download() {
        use std::io::Write;
        // A server that sends headers, then a body one byte at a time.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/", l.local_addr().unwrap().port());
        let closed = std::sync::Arc::new(AtomicBool::new(false));
        let closed2 = closed.clone();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut s, &mut buf);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: 100000\r\n\r\n");
            for _ in 0..200 {
                std::thread::sleep(Duration::from_millis(50));
                if s.write_all(b"x").and_then(|_| s.flush()).is_err() {
                    closed2.store(true, Ordering::SeqCst);
                    return;
                }
            }
        });
        let cancel = Cancel::new();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c.cancel();
        });
        let far = Instant::now() + Duration::from_secs(30);
        let r = http_get_until(&url, Dest::Stylesheet, None, false, None, far, &cancel);
        assert_eq!(r.err(), Some(NetError::Cancelled));
        // The abandoned worker stops reading and drops the connection soon
        // after, instead of downloading until its timeout.
        let start = Instant::now();
        while !closed.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(closed.load(Ordering::SeqCst), "connection still open");
    }

    #[test]
    fn web_pages_cannot_load_local_files() {
        assert!(!may_load(Some("https://evil.com/"), "file:///etc/passwd"));
        assert!(!may_load(Some("http://evil.com/"), "file:///home/me/.ssh/id_rsa"));
        assert!(may_load(Some("file:///tmp/a.html"), "file:///tmp/b.css"));
        assert!(may_load(None, "file:///tmp/a.html"));
        assert!(may_load(Some("https://a.com/"), "https://b.com/"));
        assert!(!may_load(Some("https://a.com/"), "javascript:alert(1)"));
        assert!(!may_load(None, "ftp://x/"));
    }

    #[test]
    fn local_hosts_and_fetch_site() {
        for h in [
            "localhost",
            "127.0.0.1",
            "192.168.1.10",
            "10.0.0.2",
            "[::1]",
            "printer.local",
        ] {
            assert!(is_local_host(h), "{h}");
        }
        for h in [
            "100.64.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "[fd00::1]",
            "[fe80::1]",
            "[::ffff:192.168.0.1]",
        ] {
            assert!(is_local_host(h), "{h}");
        }
        for h in [
            "example.com",
            "8.8.8.8",
            "localhost.example.com",
            "[2606:4700::1111]",
        ] {
            assert!(!is_local_host(h), "{h}");
        }
        assert_eq!(fetch_site(None, "https://a.com/"), "none");
        assert_eq!(
            fetch_site(Some("https://a.com/x"), "https://a.com/y"),
            "same-origin"
        );
        assert_eq!(
            fetch_site(Some("https://www.a.co.uk/"), "https://img.a.co.uk/"),
            "same-site"
        );
        assert_eq!(fetch_site(Some("https://a.com/"), "https://b.com/"), "cross-site");
    }

    #[test]
    fn resolver_drops_local_addresses() {
        use ureq::Resolver;
        // `localhost` resolves to loopback via /etc/hosts: no network needed.
        let err = PublicOnly.resolve("localhost:80").unwrap_err();
        assert!(err.to_string().contains(LOCAL_NETWORK_ERR));
    }

    #[test]
    fn no_proxy() {
        let l = "localhost,127.0.0.1, .corp.example,*.svc.local";
        assert!(no_proxy_matches(l, "127.0.0.1"));
        assert!(no_proxy_matches(l, "a.corp.example"));
        assert!(no_proxy_matches(l, "x.svc.local"));
        assert!(!no_proxy_matches(l, "example.com"));
        assert!(!no_proxy_matches(l, "evilcorp.example"));
    }

    #[test]
    fn address_bar_shows_real_host() {
        assert_eq!(
            display_parts("https://google.com@evil.com/x").0,
            "https://evil.com"
        );
        // Cyrillic "а" in a lookalike domain is shown as punycode.
        assert_eq!(
            display_parts("https://\u{430}pple.com/").0,
            "https://xn--pple-43d.com"
        );
        assert_eq!(display_parts("http://a.com:8080/p?q=1").1, "/p?q=1");
    }
}
