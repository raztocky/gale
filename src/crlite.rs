//! Certificate revocation checking with CRLite, as in Firefox.
//!
//! A site's certificate can be revoked before it expires (say its key was
//! stolen). rustls checks certificates but not revocation, and asking the
//! certificate authority about each site (OCSP) would tell it every site
//! you visit. CRLite instead keeps compact filters of every revoked
//! certificate on your computer: everyone downloads the same files, and
//! lookups never leave your machine.
//!
//! The filters (Mozilla's data, as republished by the rustls project's
//! upki) are downloaded by `brickle filters update`, never automatically
//! (PRIVACY.md rule 2). A certificate the filters say is revoked is refused
//! like any other invalid certificate. Without data, or for a certificate
//! the data doesn't cover yet, sites load as before.

use crate::net::{self, Dest};
use crate::network::data_dir;
use clubcard_crlite::{CRLiteClubcard, CRLiteKey, CRLiteStatus, IssuerSpkiHash, LogId, Timestamp};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where the filters are published.
const BASE_URL: &str = "https://upki.rustls.dev/revocation/";
const MANIFEST: &str = "manifest.json";
/// Limits on what the manifest may ask for.
const MAX_FILES: usize = 256;
const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;

pub fn dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("revocation"))
}

// ---------------------------------------------------------------------------
// Certificates: just enough DER to find the serial number, issuer, public
// key and Certificate Transparency timestamps
// ---------------------------------------------------------------------------

/// One DER element: its tag, contents, and the whole encoding.
struct Der<'a> {
    tag: u8,
    body: &'a [u8],
    whole: &'a [u8],
}

/// Split the first DER element off `b`.
fn der(b: &[u8]) -> Option<(Der<'_>, &[u8])> {
    let tag = *b.first()?;
    let first = *b.get(1)? as usize;
    let (len, start) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *b.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = start.checked_add(len)?;
    let body = b.get(start..end)?;
    Some((
        Der {
            tag,
            body,
            whole: &b[..end],
        },
        &b[end..],
    ))
}

/// The DER elements in `b`, in order (stops at the first malformed one).
fn elements(mut b: &[u8]) -> impl Iterator<Item = Der<'_>> {
    std::iter::from_fn(move || {
        let (e, rest) = der(b)?;
        b = rest;
        Some(e)
    })
}

/// What revocation checking needs from a certificate.
#[derive(Debug, PartialEq)]
pub struct CertInfo<'a> {
    /// The serial number's bytes, as encoded.
    pub serial: &'a [u8],
    /// The issuer's name, as encoded.
    pub issuer: &'a [u8],
    /// This certificate's own name, as encoded.
    pub subject: &'a [u8],
    /// Its SubjectPublicKeyInfo, as encoded.
    pub spki: &'a [u8],
    /// When each Certificate Transparency log saw it: (log ID, milliseconds
    /// since 1970).
    pub scts: Vec<([u8; 32], u64)>,
}

/// The X.509 extension holding embedded SCTs (1.3.6.1.4.1.11129.2.4.2).
const SCT_OID: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0xd6, 0x79, 0x02, 0x04, 0x02];
/// Most SCTs looked at per certificate (real ones have 2 or 3).
const MAX_SCTS: usize = 16;

pub fn parse_cert(cert: &[u8]) -> Option<CertInfo<'_>> {
    let (c, _) = der(cert)?;
    let (tbs, _) = der(c.body).filter(|(c, _)| c.tag == 0x30)?;
    if tbs.tag != 0x30 {
        return None;
    }
    let mut it = elements(tbs.body).peekable();
    if it.peek()?.tag == 0xa0 {
        it.next(); // version
    }
    let serial = it.next().filter(|e| e.tag == 0x02)?.body;
    let _sig = it.next().filter(|e| e.tag == 0x30)?;
    let issuer = it.next().filter(|e| e.tag == 0x30)?.whole;
    let _validity = it.next().filter(|e| e.tag == 0x30)?;
    let subject = it.next().filter(|e| e.tag == 0x30)?.whole;
    let spki = it.next().filter(|e| e.tag == 0x30)?.whole;
    let mut scts = vec![];
    if let Some(exts) = it.find(|e| e.tag == 0xa3) {
        let (list, _) = der(exts.body)?;
        for ext in elements(list.body) {
            let mut parts = elements(ext.body);
            if parts
                .next()
                .is_some_and(|oid| oid.tag == 0x06 && oid.body == SCT_OID)
            {
                if let Some(value) = parts.find(|p| p.tag == 0x04) {
                    scts = parse_scts(value.body).unwrap_or_default();
                }
            }
        }
    }
    Some(CertInfo {
        serial,
        issuer,
        subject,
        spki,
        scts,
    })
}

/// The log IDs and timestamps in a SignedCertificateTimestampList (RFC
/// 6962 §3.3), itself wrapped in an OCTET STRING.
fn parse_scts(value: &[u8]) -> Option<Vec<([u8; 32], u64)>> {
    let (inner, _) = der(value).filter(|(e, _)| e.tag == 0x04)?;
    let b = inner.body;
    let total = u16::from_be_bytes([*b.first()?, *b.get(1)?]) as usize;
    let mut list = b.get(2..2 + total)?;
    let mut out = vec![];
    while list.len() >= 2 && out.len() < MAX_SCTS {
        let len = u16::from_be_bytes([list[0], list[1]]) as usize;
        let sct = list.get(2..2 + len)?;
        list = &list[2 + len..];
        // Version 1 (v1 = 0): log ID, then the timestamp.
        if sct.first() == Some(&0) && sct.len() >= 41 {
            let id: [u8; 32] = sct[1..33].try_into().ok()?;
            let ts = u64::from_be_bytes(sct[33..41].try_into().ok()?);
            out.push((id, ts));
        }
    }
    Some(out)
}

/// DER-encode a SEQUENCE around `body` (trust anchors store their public
/// key without it).
fn sequence(body: &[u8]) -> Vec<u8> {
    let mut out = vec![0x30];
    let len = body.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes: Vec<u8> = len.to_be_bytes().into_iter().skip_while(|&b| b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend(body);
    out
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    for p in parts {
        ctx.update(p);
    }
    ctx.finish().as_ref().try_into().expect("SHA-256 is 32 bytes")
}

// ---------------------------------------------------------------------------
// Looking certificates up
// ---------------------------------------------------------------------------

/// The filters, loaded once per run from what `brickle filters update`
/// saved.
struct Store {
    filters: Vec<CRLiteClubcard>,
}

fn store() -> Option<&'static Store> {
    static STORE: OnceLock<Option<Store>> = OnceLock::new();
    STORE.get_or_init(|| dir().and_then(|d| load(&d))).as_ref()
}

fn load(dir: &Path) -> Option<Store> {
    let manifest = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let files = parse_manifest(&manifest).ok()?;
    let mut filters = vec![];
    for f in files {
        let bytes = std::fs::read(dir.join(&f.name)).ok()?;
        // Only files the manifest vouches for.
        if bytes.len() != f.size || sha256(&[&bytes]) != f.hash {
            return None;
        }
        filters.push(CRLiteClubcard::from_bytes(&bytes).ok()?);
    }
    (!filters.is_empty()).then_some(Store { filters })
}

/// What the filters say about a certificate.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Status {
    Revoked,
    NotRevoked,
    /// Too new for the data, not logged in Certificate Transparency, or no
    /// data downloaded.
    Unknown,
}

/// Look a certificate up, given the certificates that could have issued it
/// (the chain the server sent and your trusted roots).
pub fn check(end_entity: &[u8], intermediates: &[&[u8]], roots: &[(Vec<u8>, Vec<u8>)]) -> Status {
    match store() {
        Some(s) => check_in(&s.filters, end_entity, intermediates, roots),
        None => Status::Unknown,
    }
}

/// [`check`] against these filters. `roots` are (subject, public key info).
fn check_in(
    filters: &[CRLiteClubcard],
    end_entity: &[u8],
    intermediates: &[&[u8]],
    roots: &[(Vec<u8>, Vec<u8>)],
) -> Status {
    let Some(ee) = parse_cert(end_entity) else {
        return Status::Unknown;
    };
    if ee.scts.is_empty() {
        return Status::Unknown;
    }
    // The issuer's public key: from the chain, else a trusted root. (rustls
    // has already checked the chain, so a certificate with this name that
    // was sent along is the issuer.)
    let mut keys: Vec<Vec<u8>> = intermediates
        .iter()
        .filter_map(|c| parse_cert(c))
        .filter(|c| c.subject == ee.issuer)
        .map(|c| c.spki.to_vec())
        .collect();
    keys.extend(
        roots
            .iter()
            .filter(|(s, _)| s == ee.issuer)
            .map(|(_, k)| k.clone()),
    );
    let mut status = Status::Unknown;
    for key in keys.iter().take(8) {
        let issuer = IssuerSpkiHash(sha256(&[key]));
        let key = CRLiteKey::with_hash(&issuer, ee.serial, sha256(&[&issuer.0, ee.serial]));
        for f in filters {
            let stamps = ee.scts.iter().map(|(id, ts)| (LogId(*id), Timestamp(*ts)));
            match f.contains(&key, stamps) {
                CRLiteStatus::Revoked => return Status::Revoked,
                CRLiteStatus::Good => status = Status::NotRevoked,
                CRLiteStatus::NotCovered | CRLiteStatus::NotEnrolled => {}
            }
        }
    }
    status
}

/// Checks certificates the usual way (rustls' WebPKI verifier, your
/// trusted roots), then refuses ones the CRLite filters say are revoked.
/// Everything else, including what the TLS handshake offers, is the
/// standard verifier's.
#[derive(Debug)]
pub struct Verifier {
    inner: std::sync::Arc<rustls::client::WebPkiServerVerifier>,
    /// Trusted roots as (subject, SubjectPublicKeyInfo), DER.
    roots: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Verifier {
    pub fn new(roots: std::sync::Arc<rustls::RootCertStore>) -> Result<Verifier, String> {
        let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(
            roots.clone(),
            rustls::crypto::ring::default_provider().into(),
        )
        .build()
        .map_err(|e| e.to_string())?;
        let roots = roots
            .roots
            .iter()
            .map(|a| (sequence(&a.subject), sequence(&a.subject_public_key_info)))
            .collect();
        Ok(Verifier { inner, roots })
    }
}

impl rustls::client::danger::ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let ok = self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)?;
        let chain: Vec<&[u8]> = intermediates.iter().map(|c| c.as_ref()).collect();
        match check(end_entity, &chain, &self.roots) {
            Status::Revoked => Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::Revoked,
            )),
            Status::NotRevoked | Status::Unknown => Ok(ok),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        self.inner.requires_raw_public_keys()
    }

    fn root_hint_subjects(&self) -> Option<&[rustls::DistinguishedName]> {
        self.inner.root_hint_subjects()
    }
}

// ---------------------------------------------------------------------------
// Downloading (`brickle filters update`)
// ---------------------------------------------------------------------------

/// A file the manifest lists.
#[derive(Debug, PartialEq)]
struct FileEntry {
    name: String,
    size: usize,
    hash: [u8; 32],
}

/// The files in a manifest, checked: safe names, sane sizes.
fn parse_manifest(text: &str) -> Result<Vec<FileEntry>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("manifest: {e}"))?;
    let files = v
        .get("files")
        .or_else(|| v.get("filters"))
        .and_then(|f| f.as_array())
        .ok_or("manifest lists no files")?;
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(format!("manifest lists {} files", files.len()));
    }
    let mut out = vec![];
    let mut total = 0usize;
    for f in files {
        let name = f.get("filename").and_then(|n| n.as_str()).unwrap_or("");
        let safe = !name.is_empty()
            && name.len() <= 64
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            && (name.ends_with(".filter") || name.ends_with(".delta"));
        if !safe {
            return Err(format!("manifest names an unexpected file {name:?}"));
        }
        let size = f
            .get("size")
            .and_then(|s| s.as_u64())
            .filter(|&s| s > 0 && s as usize <= MAX_FILE_BYTES)
            .ok_or_else(|| format!("{name}: bad size"))? as usize;
        let hex = f.get("hash").and_then(|h| h.as_str()).unwrap_or("");
        let mut hash = [0u8; 32];
        if hex.len() != 64 {
            return Err(format!("{name}: bad hash"));
        }
        for (i, b) in hash.iter_mut().enumerate() {
            *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2).unwrap_or("zz"), 16)
                .map_err(|_| format!("{name}: bad hash"))?;
        }
        total += size;
        if total > MAX_TOTAL_BYTES {
            return Err("manifest asks for too much data".into());
        }
        out.push(FileEntry {
            name: name.to_string(),
            size,
            hash,
        });
    }
    Ok(out)
}

/// GET over https, following redirects within https, printing each request.
fn download(url: &str, log: &mut dyn FnMut(&str)) -> Result<Vec<u8>, String> {
    let mut url = url.to_string();
    for _ in 0..5 {
        if net::scheme(&url) != "https" {
            log(&format!("  refused  {url} (not https)"));
            return Err("refusing non-https download".into());
        }
        match net::http_get(&url, Dest::Download, None, true, None) {
            Ok(r) if (300..400).contains(&r.status) => {
                let next = net::resolve(&url, r.location.as_deref().unwrap_or(""));
                log(&format!("  GET {url} -> {} redirect to {next}", r.status));
                url = next;
            }
            Ok(r) if r.status == 200 => {
                let bytes = if r.bytes.is_empty() {
                    r.body.into_bytes()
                } else {
                    r.bytes
                };
                log(&format!("  GET {url} -> 200, {} KB", bytes.len() / 1024));
                return Ok(bytes);
            }
            Ok(r) => {
                log(&format!("  GET {url} -> {}", r.status));
                return Err(format!("HTTP {}", r.status));
            }
            Err(e) => {
                log(&format!("  GET {url} -> failed: {e}"));
                return Err(e.to_string());
            }
        }
    }
    Err("too many redirects".into())
}

/// Write `bytes` to `path` so a reader sees the old file or the new one.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Bring the local filters up to date: fetch the manifest, download the
/// files that changed (each checked against its hash), and remove old
/// ones. Every request is printed.
pub fn update(log: &mut dyn FnMut(&str)) -> Result<(), String> {
    let dir = dir().ok_or("can't find a data directory (is $HOME set?)")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let manifest = download(&format!("{BASE_URL}{MANIFEST}"), log)?;
    let text = String::from_utf8(manifest).map_err(|_| "manifest isn't text")?;
    let files = parse_manifest(&text)?;
    for f in &files {
        let path = dir.join(&f.name);
        if std::fs::read(&path).is_ok_and(|b| b.len() == f.size && sha256(&[&b]) == f.hash) {
            continue;
        }
        let bytes = download(&format!("{BASE_URL}{}", f.name), log)?;
        if bytes.len() != f.size || sha256(&[&bytes]) != f.hash {
            return Err(format!("{}: doesn't match the manifest", f.name));
        }
        CRLiteClubcard::from_bytes(&bytes).map_err(|e| format!("{}: not a CRLite filter ({e:?})", f.name))?;
        write_atomic(&path, &bytes)?;
    }
    // The manifest last, so a half-done update leaves the old set in use.
    write_atomic(&dir.join(MANIFEST), text.as_bytes())?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let stale = (name.ends_with(".filter") || name.ends_with(".delta") || name.ends_with(".part"))
                && !files.iter().any(|f| f.name == name);
            if stale {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// For `brickle filters status`: how old the downloaded data is.
pub fn status() -> String {
    let manifest = dir().and_then(|d| std::fs::read_to_string(d.join(MANIFEST)).ok());
    let generated = manifest
        .as_deref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| v.get("generated_at")?.as_u64());
    match generated {
        None => "Revocation data (CRLite): not downloaded; revoked certificates aren't caught".into(),
        Some(t) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let hours = now.saturating_sub(t) / 3600;
            format!(
                "Revocation data (CRLite): {} old{}",
                if hours < 48 {
                    format!("{hours} hours")
                } else {
                    format!("{} days", hours / 24)
                },
                if hours > 24 * 7 {
                    "; certificates revoked since then aren't caught"
                } else {
                    ""
                }
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// example.com's certificate (with embedded SCTs) and its issuer, as
    /// served in November 2026.
    const EE: &[u8] = include_bytes!("../tests/data/example-com.der");
    const ISSUER: &[u8] = include_bytes!("../tests/data/example-com-issuer.der");

    #[test]
    fn reads_what_revocation_needs_from_a_certificate() {
        let ee = parse_cert(EE).unwrap();
        let issuer = parse_cert(ISSUER).unwrap();
        assert_eq!(ee.issuer, issuer.subject);
        assert!(!ee.serial.is_empty());
        assert!((2..=5).contains(&ee.scts.len()), "{}", ee.scts.len());
        // Logged after 2020 and before 2100.
        for (_, ts) in &ee.scts {
            assert!((1_577_836_800_000..4_102_444_800_000).contains(ts), "{ts}");
        }
        // The public key info is a whole DER SEQUENCE.
        assert_eq!(issuer.spki[0], 0x30);
        assert_eq!(der(issuer.spki).unwrap().1, &[] as &[u8]);
    }

    #[test]
    fn junk_certificates_are_unknown() {
        for n in 0..EE.len() {
            let _ = parse_cert(&EE[..n]);
        }
        assert_eq!(check_in(&[], b"junk", &[], &[]), Status::Unknown);
        assert_eq!(check_in(&[], EE, &[ISSUER], &[]), Status::Unknown);
    }

    #[test]
    fn sequence_lengths() {
        for len in [0, 5, 127, 128, 255, 256, 70000] {
            let body = vec![7u8; len];
            let s = sequence(&body);
            let (e, rest) = der(&s).unwrap();
            assert_eq!((e.tag, e.body.len(), rest.len()), (0x30, len, 0));
        }
    }

    #[test]
    fn manifests_are_checked() {
        let ok = r#"{"generated_at":1,"comment":"x","files":[{"filename":"20261017-1-default.filter","size":5,"hash":"524dc88862064d70fdee3fa94a6196aaadf7ebd4970710078239ceb041640fc8"}]}"#;
        let files = parse_manifest(ok).unwrap();
        assert_eq!(files[0].name, "20261017-1-default.filter");
        assert_eq!(files[0].hash[0], 0x52);
        for bad in [
            ok.replace("20261017-1-default.filter", "../x.filter"),
            ok.replace("20261017-1-default.filter", "x.exe"),
            ok.replace("20261017-1-default.filter", ".filter"),
            ok.replace("\"size\":5", "\"size\":0"),
            ok.replace("\"size\":5", "\"size\":99999999999"),
            ok.replace("524d", "zz4d"),
            ok.replace("\"files\"", "\"other\""),
            "not json".to_string(),
        ] {
            assert!(parse_manifest(&bad).is_err(), "{bad}");
        }
    }
}
