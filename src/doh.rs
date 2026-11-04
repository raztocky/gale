//! DNS over HTTPS (RFC 8484), so your network and ISP can't read (or
//! quietly change) which sites you look up.
//!
//! Lookups go to one resolver you choose (`brickle dns`), over HTTPS to an
//! address built in here, so finding the resolver needs no plain DNS
//! either. If it can't be reached, the lookup fails: Brickle never falls
//! back to plain-text DNS on its own.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A DNS-over-HTTPS resolver.
#[derive(Debug, PartialEq, Eq)]
pub struct Provider {
    /// What `brickle dns` calls it.
    pub name: &'static str,
    /// Its host name, as in its certificate.
    pub host: &'static str,
    /// Where to send queries.
    pub url: &'static str,
    /// Its fixed addresses, so reaching it needs no DNS.
    pub addrs: &'static [IpAddr],
    /// Who runs it, for `brickle dns`.
    pub about: &'static str,
}

const fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

#[allow(clippy::too_many_arguments)]
const fn v6(a: u16, b: u16, c: u16, d: u16, e: u16, f: u16, g: u16, h: u16) -> IpAddr {
    IpAddr::V6(Ipv6Addr::new(a, b, c, d, e, f, g, h))
}

/// The resolvers you can pick. The first is the default, as in Firefox.
/// (Quad9 and Mullvad only answer over HTTP/2, which Brickle doesn't speak
/// yet, so they can't be offered.)
pub const PROVIDERS: [Provider; 3] = [
    Provider {
        name: "cloudflare",
        host: "cloudflare-dns.com",
        url: "https://cloudflare-dns.com/dns-query",
        addrs: &[
            v4(1, 1, 1, 1),
            v4(1, 0, 0, 1),
            v6(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111),
            v6(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1001),
        ],
        about: "Cloudflare, Firefox's default; deletes its logs within 25 hours, audited yearly",
    },
    Provider {
        name: "wikimedia",
        host: "wikimedia-dns.org",
        url: "https://wikimedia-dns.org/dns-query",
        addrs: &[v4(185, 71, 138, 138), v6(0x2001, 0x67c, 0x930, 0, 0, 0, 0, 0x1)],
        about: "Wikimedia DNS, run by the non-profit behind Wikipedia; keeps no IP addresses",
    },
    Provider {
        name: "adguard",
        host: "unfiltered.adguard-dns.com",
        url: "https://unfiltered.adguard-dns.com/dns-query",
        addrs: &[
            v4(94, 140, 14, 140),
            v4(94, 140, 14, 141),
            v6(0x2a10, 0x50c0, 0, 0, 0, 0, 0x1, 0xff),
            v6(0x2a10, 0x50c0, 0, 0, 0, 0, 0x2, 0xff),
        ],
        about: "AdGuard DNS (unfiltered), based in Cyprus; says it keeps no logs",
    },
];

/// What `brickle dns system` saves: use the system's resolver.
pub const SYSTEM: &str = "system";

fn setting_file() -> Option<std::path::PathBuf> {
    crate::network::config_dir().map(|d| d.join("dns"))
}

/// Parse a saved or typed setting: a provider, `system` (None), or an error.
pub fn parse_setting(s: &str) -> Result<Option<&'static Provider>, String> {
    let s = s.trim().to_ascii_lowercase();
    if s == SYSTEM {
        return Ok(None);
    }
    PROVIDERS.iter().find(|p| p.name == s).map(Some).ok_or_else(|| {
        let names: Vec<_> = PROVIDERS.iter().map(|p| p.name).collect();
        format!(
            "unknown resolver {s:?}: pick one of {}, or {SYSTEM}",
            names.join(", ")
        )
    })
}

/// The resolver in use: what you saved with `brickle dns`, else the default.
/// Read once per run.
pub fn provider() -> Option<&'static Provider> {
    static CHOSEN: OnceLock<Option<&'static Provider>> = OnceLock::new();
    *CHOSEN.get_or_init(|| {
        let saved = setting_file().and_then(|f| std::fs::read_to_string(f).ok());
        match saved.map(|s| parse_setting(&s)) {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                // A broken setting must not silently mean plain DNS.
                eprintln!("brickle: dns setting: {e}; using {}", PROVIDERS[0].name);
                Some(&PROVIDERS[0])
            }
            None => Some(&PROVIDERS[0]),
        }
    })
}

/// Save a setting for future runs.
pub fn save_setting(p: Option<&Provider>) -> Result<(), String> {
    let f = setting_file().ok_or("no config directory (HOME isn't set)")?;
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&f, format!("{}\n", p.map_or(SYSTEM, |p| p.name))).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Wire format (RFC 1035), just enough for A and AAAA lookups
// ---------------------------------------------------------------------------

pub const TYPE_A: u16 = 1;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_AAAA: u16 = 28;
const TYPE_OPT: u16 = 41;
/// Queries are padded to a multiple of this (RFC 8467), so their size
/// doesn't give away how long the name is.
const PAD_TO: usize = 128;
/// Largest answer we read.
pub const MAX_RESPONSE: usize = 16 * 1024;
/// Most CNAME hops followed within one answer.
const MAX_CHAIN: usize = 8;
/// Most addresses kept per name.
const MAX_ADDRS: usize = 16;

/// A query for `name`'s records of type `qtype`, padded. None if the name
/// isn't a valid DNS name.
pub fn query(name: &str, qtype: u16) -> Option<Vec<u8>> {
    let name = name.trim_end_matches('.');
    if name.is_empty() || name.len() > 253 {
        return None;
    }
    // Header: ID 0 (RFC 8484 §4.1), recursion desired, one question, one
    // additional record (the OPT record carrying the padding).
    let mut q = vec![0, 0, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1];
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 || !label.bytes().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        q.push(label.len() as u8);
        q.extend(label.bytes().map(|b| b.to_ascii_lowercase()));
    }
    q.push(0);
    q.extend(qtype.to_be_bytes());
    q.extend(1u16.to_be_bytes()); // class IN
                                  // OPT: root name, type, UDP size 1232, no extended flags, then the
                                  // padding option (code 12) filling the query out to a multiple of PAD_TO.
    q.push(0);
    q.extend(TYPE_OPT.to_be_bytes());
    q.extend(1232u16.to_be_bytes());
    q.extend([0, 0, 0, 0]);
    let fixed = q.len() + 2 + 4; // RDLENGTH, then the option's code and length
    let pad = (PAD_TO - fixed % PAD_TO) % PAD_TO;
    q.extend(((4 + pad) as u16).to_be_bytes());
    q.extend(12u16.to_be_bytes());
    q.extend((pad as u16).to_be_bytes());
    q.extend(std::iter::repeat_n(0, pad));
    Some(q)
}

/// What an answer said.
#[derive(Debug, PartialEq)]
pub enum Answer {
    /// The addresses found (maybe none) and how long they may be kept.
    Addrs(Vec<IpAddr>, Duration),
    /// The name doesn't exist.
    NoSuchName,
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Read the (possibly compressed) name at `i`. Returns it lowercased and
/// where the bytes after it start.
fn read_name(b: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut end = None;
    // Each pointer must go backwards, so this always ends; the hop limit
    // bounds the work anyway.
    for _ in 0..128 {
        let len = *b.get(i)? as usize;
        match len {
            0 => {
                return Some((name, end.unwrap_or(i + 1)));
            }
            l if l & 0xc0 == 0xc0 => {
                let target = (u16_at(b, i)? & 0x3fff) as usize;
                if target >= i {
                    return None;
                }
                end.get_or_insert(i + 2);
                i = target;
            }
            l if l <= 63 => {
                let label = b.get(i + 1..i + 1 + l)?;
                if !name.is_empty() {
                    name.push('.');
                }
                if name.len() + l > 253 {
                    return None;
                }
                name.extend(label.iter().map(|c| c.to_ascii_lowercase() as char));
                i += 1 + l;
            }
            _ => return None,
        }
    }
    None
}

/// Parse the answer to a [`query`] for `name` of type `qtype`. None if it
/// isn't a well-formed answer to that question (or the server failed).
pub fn parse(b: &[u8], name: &str, qtype: u16) -> Option<Answer> {
    if b.len() > MAX_RESPONSE || b.len() < 12 {
        return None;
    }
    let flags = u16_at(b, 2)?;
    let (qr, opcode, tc, rcode) = (flags >> 15, (flags >> 11) & 0xf, (flags >> 9) & 1, flags & 0xf);
    if u16_at(b, 0)? != 0 || qr != 1 || opcode != 0 || tc != 0 {
        return None;
    }
    let (qd, an) = (u16_at(b, 4)?, u16_at(b, 6)?);
    if qd != 1 {
        return None;
    }
    // The question must be the one we asked.
    let want = name.trim_end_matches('.').to_ascii_lowercase();
    let (qname, mut i) = read_name(b, 12)?;
    if qname != want || u16_at(b, i)? != qtype || u16_at(b, i + 2)? != 1 {
        return None;
    }
    i += 4;
    match rcode {
        0 => {}
        3 => return Some(Answer::NoSuchName),
        _ => return None,
    }
    // Addresses for the name, or for the names it's an alias of.
    let mut names = vec![want];
    let mut addrs = Vec::new();
    let mut ttl = u32::MAX;
    for _ in 0..an {
        let (owner, j) = read_name(b, i)?;
        let (rtype, class, rttl, len) = (
            u16_at(b, j)?,
            u16_at(b, j + 2)?,
            u32_at(b, j + 4)?,
            u16_at(b, j + 8)?,
        );
        let data = j + 10;
        let rdata = b.get(data..data + len as usize)?;
        i = data + len as usize;
        if class != 1 || !names.contains(&owner) {
            continue;
        }
        match (rtype, rdata.len()) {
            (TYPE_CNAME, _) if names.len() <= MAX_CHAIN => {
                let (target, _) = read_name(b, data)?;
                if !names.contains(&target) {
                    names.push(target);
                }
                ttl = ttl.min(rttl);
            }
            (t, 4) if t == qtype && t == TYPE_A => {
                addrs.push(IpAddr::from(<[u8; 4]>::try_from(rdata).ok()?));
                ttl = ttl.min(rttl);
            }
            (t, 16) if t == qtype && t == TYPE_AAAA => {
                addrs.push(IpAddr::from(<[u8; 16]>::try_from(rdata).ok()?));
                ttl = ttl.min(rttl);
            }
            _ => {}
        }
        if addrs.len() >= MAX_ADDRS {
            break;
        }
    }
    Some(Answer::Addrs(addrs, Duration::from_secs(ttl as u64)))
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

/// Answers are kept at least this long (fewer lookups) and at most this
/// long (sites that move are found again).
const MIN_TTL: Duration = Duration::from_secs(60);
const MAX_TTL: Duration = Duration::from_secs(3600);
/// Names whose answers are kept at once.
const MAX_CACHED: usize = 2048;

type Cache = HashMap<String, (Result<Vec<IpAddr>, String>, Instant)>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn ask(p: &'static Provider, name: &str, qtype: u16) -> Result<Answer, String> {
    let q = query(name, qtype).ok_or_else(|| format!("{name:?} isn't a valid host name"))?;
    let body = crate::net::doh_exchange(p, &q)?;
    parse(&body, name, qtype).ok_or_else(|| "the resolver's answer didn't make sense".into())
}

/// `host`'s addresses (IPv4 first), looked up with `p`.
pub fn lookup(p: &'static Provider, host: &str) -> Result<Vec<IpAddr>, String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let key = format!("{} {host}", p.name);
    {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        match c.get(&key) {
            Some((r, until)) if *until > Instant::now() => return r.clone(),
            Some(_) => {
                c.remove(&key);
            }
            None => {}
        }
    }
    // Both lookups at once: each is a round trip to the resolver.
    let h6 = host.clone();
    let six = std::thread::Builder::new()
        .name("brickle-dns".into())
        .spawn(move || ask(p, &h6, TYPE_AAAA));
    let four = ask(p, &host, TYPE_A);
    let six = six
        .map_err(|e| e.to_string())
        .and_then(|t| t.join().map_err(|_| "lookup failed".to_string()))
        .and_then(|r| r);
    let (result, ttl) = match (four, six) {
        (Ok(Answer::NoSuchName), _) | (_, Ok(Answer::NoSuchName)) => {
            (Err(format!("{host} doesn't exist (no such name)")), MIN_TTL)
        }
        (Ok(Answer::Addrs(a, t4)), Ok(Answer::Addrs(b, t6))) => {
            let all: Vec<_> = a.into_iter().chain(b).collect();
            if all.is_empty() {
                (Err(format!("{host} has no addresses")), MIN_TTL)
            } else {
                (Ok(all), t4.min(t6))
            }
        }
        // One kind of address is enough (many sites have no IPv6).
        (Ok(Answer::Addrs(a, t)), Err(_)) | (Err(_), Ok(Answer::Addrs(a, t))) if !a.is_empty() => (Ok(a), t),
        (Err(e), _) | (_, Err(e)) => {
            // Not cached: the network may come back.
            return Err(format!(
                "couldn't look up {host} with DNS over HTTPS ({}: {e}). Brickle doesn't fall back \
                 to plain DNS; `brickle dns` picks another resolver, or `brickle dns system` uses \
                 your system's",
                p.host
            ));
        }
    };
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.len() >= MAX_CACHED {
        let now = Instant::now();
        c.retain(|_, (_, until)| *until > now);
        if c.len() >= MAX_CACHED {
            c.clear();
        }
    }
    c.insert(
        key,
        (result.clone(), Instant::now() + ttl.clamp(MIN_TTL, MAX_TTL)),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer to `query(name, qtype)` with these records (owner offset
    /// 12 = the question's name, via a pointer).
    fn answer(name: &str, qtype: u16, rcode: u16, records: &[(u16, u32, Vec<u8>)]) -> Vec<u8> {
        let q = query(name, qtype).unwrap();
        let qend = 12 + q[12..].iter().position(|&b| b == 0).unwrap() + 1 + 4;
        let mut b = vec![0, 0];
        b.extend((0x8180u16 | rcode).to_be_bytes());
        b.extend([0, 1]);
        b.extend((records.len() as u16).to_be_bytes());
        b.extend([0, 0, 0, 0]);
        b.extend(&q[12..qend]);
        for (t, ttl, data) in records {
            b.extend([0xc0, 12]);
            b.extend(t.to_be_bytes());
            b.extend([0, 1]);
            b.extend(ttl.to_be_bytes());
            b.extend((data.len() as u16).to_be_bytes());
            b.extend(data);
        }
        b
    }

    #[test]
    fn queries_are_padded_and_well_formed() {
        for name in [
            "a.io",
            "example.com",
            &"x".repeat(60),
            "very.long.sub.domain.example.org.",
        ] {
            let q = query(name, TYPE_A).unwrap();
            assert_eq!(q.len() % PAD_TO, 0, "{name}");
            assert_eq!(&q[..2], &[0, 0], "ID 0");
        }
        assert!(query("", TYPE_A).is_none());
        assert!(query("a..b", TYPE_A).is_none());
        assert!(query("a b.com", TYPE_A).is_none());
        assert!(query(&format!("{}.com", "x".repeat(64)), TYPE_A).is_none());
    }

    #[test]
    fn parses_addresses_and_aliases() {
        let b = answer("Example.com", TYPE_A, 0, &[(TYPE_A, 300, vec![93, 184, 215, 14])]);
        assert_eq!(
            parse(&b, "example.com", TYPE_A),
            Some(Answer::Addrs(
                vec![v4(93, 184, 215, 14)],
                Duration::from_secs(300)
            ))
        );
        // www -> cdn (CNAME, written out at offset `cdn`), then cdn's address.
        let mut b = answer("www.a.com", TYPE_AAAA, 0, &[]);
        b[7] = 2; // two answers
        let cdn_at = b.len() + 12;
        b.extend([0xc0, 12, 0, 5, 0, 1, 0, 0, 0, 60]);
        let target = [3, b'c', b'd', b'n', 1, b'b', 3, b'n', b'e', b't', 0];
        b.extend((target.len() as u16).to_be_bytes());
        b.extend(target);
        b.extend([0xc0, cdn_at as u8, 0, 28, 0, 1, 0, 0, 1, 0, 0, 16]);
        b.extend([0x20, 1, 0xd, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(
            parse(&b, "www.a.com", TYPE_AAAA),
            Some(Answer::Addrs(
                vec!["2001:db8::1".parse().unwrap()],
                Duration::from_secs(60)
            ))
        );
    }

    #[test]
    fn rejects_answers_to_other_questions_and_junk() {
        let b = answer("a.com", TYPE_A, 0, &[(TYPE_A, 300, vec![1, 2, 3, 4])]);
        assert!(parse(&b, "b.com", TYPE_A).is_none());
        assert!(parse(&b, "a.com", TYPE_AAAA).is_none());
        // Records for some other name are ignored, not believed.
        let mut other = answer("a.com", TYPE_A, 0, &[]);
        other[7] = 1;
        other.extend([1, b'z', 0, 0, 1, 0, 1, 0, 0, 0, 9, 0, 4, 6, 6, 6, 6]);
        assert_eq!(
            parse(&other, "a.com", TYPE_A),
            Some(Answer::Addrs(vec![], Duration::from_secs(u32::MAX as u64)))
        );
        assert_eq!(
            parse(&answer("gone.com", TYPE_A, 3, &[]), "gone.com", TYPE_A),
            Some(Answer::NoSuchName)
        );
        assert!(
            parse(&answer("a.com", TYPE_A, 2, &[]), "a.com", TYPE_A).is_none(),
            "SERVFAIL"
        );
        for n in 0..b.len() {
            let _ = parse(&b[..n], "a.com", TYPE_A);
        }
        // A pointer loop.
        let mut lp = b.clone();
        let at = lp.len();
        lp[7] = 2;
        lp.extend([0xc0, at as u8, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 1, 1, 1, 1]);
        let _ = parse(&lp, "a.com", TYPE_A);
    }

    /// Real lookups with each resolver (needs the internet).
    #[test]
    #[ignore]
    fn live_lookups() {
        for p in &PROVIDERS {
            let a = lookup(p, "example.com").unwrap_or_else(|e| panic!("{}: {e}", p.name));
            assert!(!a.is_empty());
            println!("{}: {a:?}", p.name);
            assert!(lookup(p, "nonexistent.invalid")
                .unwrap_err()
                .contains("doesn't exist"));
        }
    }

    #[test]
    fn settings() {
        assert_eq!(parse_setting("Wikimedia\n"), Ok(Some(&PROVIDERS[1])));
        assert_eq!(parse_setting("system"), Ok(None));
        assert!(parse_setting("8.8.8.8").is_err());
    }
}
