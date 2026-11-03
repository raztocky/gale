//! Strip tracking parameters from URLs before they are requested.
//!
//! Built-in list based on the parameters Firefox, LibreWolf, DuckDuckGo
//! and uBlock Origin strip, plus any `$removeparam` rules from the filter
//! lists (see [`crate::filter`]).

/// Parameter names removed everywhere.
const EXACT: &[&str] = &[
    "fbclid",
    "gclid",
    "gclsrc",
    "dclid",
    "gbraid",
    "wbraid",
    "gad_source",
    "srsltid",
    "msclkid",
    "mc_cid",
    "mc_eid",
    "igshid",
    "igsh",
    "yclid",
    "ysclid",
    "twclid",
    "ttclid",
    "li_fat_id",
    "_hsenc",
    "_hsmi",
    "__hssc",
    "__hstc",
    "__hsfp",
    "hsctatracking",
    "mkt_tok",
    "oly_anon_id",
    "oly_enc_id",
    "vero_conv",
    "vero_id",
    "wickedid",
    "rb_clickid",
    "s_cid",
    "_openstat",
    "__s",
    "_ga",
    "_gl",
    "epik",
    "pk_campaign",
    "pk_kwd",
    "pk_source",
    "pk_medium",
    "piwik_campaign",
    "piwik_kwd",
    "mtm_campaign",
    "mtm_source",
    "mtm_medium",
    "mtm_keyword",
    "zanpid",
    "irclickid",
    "cvid",
    "oicd",
    "ref_src",
    "ref_url",
];
/// Parameter-name prefixes removed everywhere.
const PREFIXES: &[&str] = &["utm_", "stm_", "hsa_"];

pub fn is_tracking_param(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    EXACT.contains(&n.as_str()) || PREFIXES.iter().any(|p| n.starts_with(p))
}

/// Remove query parameters for which `strip(name)` is true. Returns the new
/// URL and the names removed. Non-http(s) URLs are returned unchanged.
pub fn strip_params(url: &str, strip: &dyn Fn(&str) -> bool) -> (String, Vec<String>) {
    let Ok(mut u) = url::Url::parse(url) else {
        return (url.to_string(), vec![]);
    };
    if !matches!(u.scheme(), "http" | "https") || u.query().is_none() {
        return (url.to_string(), vec![]);
    }
    let mut removed = Vec::new();
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| {
            let drop = strip(k);
            if drop {
                removed.push(k.to_string());
            }
            !drop
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if removed.is_empty() {
        return (url.to_string(), removed);
    }
    if kept.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(kept);
    }
    (u.to_string(), removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_known_trackers_only() {
        let (u, r) = strip_params(
            "https://shop.example/item?id=42&utm_source=x&UTM_Medium=y&fbclid=abc#top",
            &is_tracking_param,
        );
        assert_eq!(u, "https://shop.example/item?id=42#top");
        assert_eq!(r, vec!["utm_source", "UTM_Medium", "fbclid"]);

        let (u, r) = strip_params("https://a.example/?gclid=1", &is_tracking_param);
        assert_eq!(u, "https://a.example/");
        assert_eq!(r.len(), 1);

        let clean = "https://a.example/search?q=utm_source&page=2";
        assert_eq!(
            strip_params(clean, &is_tracking_param),
            (clean.to_string(), vec![])
        );
        assert_eq!(
            strip_params("file:///x?utm_source=1", &is_tracking_param).1.len(),
            0
        );
    }
}
