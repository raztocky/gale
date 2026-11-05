//! What a request was for, in plain words, for the request panel: who it
//! goes to (a table of well-known trackers, written by hand), what it
//! carried (cookies, the page's address, identifiers), and which filter
//! list blocked it.
//!
//! It only says what Brickle can see. What a company does with the data on
//! its servers can't be seen, so the descriptions say what each tracker
//! is *for*, as its maker describes it.
use crate::network::{LogEntry, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Advertising,
    Analytics,
    SessionReplay,
    Social,
    TagManager,
    Identity,
    Fingerprinting,
    Monitoring,
    Consent,
    Content,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Advertising => "advertising",
            Kind::Analytics => "analytics",
            Kind::SessionReplay => "session recording",
            Kind::Social => "social media",
            Kind::TagManager => "tag manager",
            Kind::Identity => "cross-site identity",
            Kind::Fingerprinting => "fingerprinting",
            Kind::Monitoring => "performance and error monitoring",
            Kind::Consent => "cookie consent",
            Kind::Content => "third-party content",
        }
    }
}

pub struct Tracker {
    /// A host and its subdomains, optionally with a path prefix
    /// (`facebook.com/tr`).
    pub at: &'static str,
    pub name: &'static str,
    pub company: &'static str,
    pub kind: Kind,
    /// What it's for, finishing "it …".
    pub what: &'static str,
}

const fn t(
    at: &'static str,
    name: &'static str,
    company: &'static str,
    kind: Kind,
    what: &'static str,
) -> Tracker {
    Tracker {
        at,
        name,
        company,
        kind,
        what,
    }
}

use Kind::*;

/// Well-known third parties. More specific entries (with a path) first.
pub const TRACKERS: &[Tracker] = &[
    t("facebook.com/tr", "Meta Pixel", "Meta", Advertising, "tells Meta which pages you visit and what you do on them, to target and measure ads, whether or not you use Facebook"),
    t("facebook.com/plugins", "Facebook social plugin", "Meta", Social, "shows a Like or share button, and lets Meta see that you visited this page"),
    t("connect.facebook.net", "Meta Pixel / Facebook SDK", "Meta", Advertising, "loads Meta's tracking code, which reports your visit and actions on this page to Meta for ads"),
    t("google-analytics.com", "Google Analytics", "Google", Analytics, "records which pages you view, for how long and where you came from, under an ID kept in a cookie"),
    t("analytics.google.com", "Google Analytics", "Google", Analytics, "records which pages you view, for how long and where you came from, under an ID kept in a cookie"),
    t("googletagmanager.com", "Google Tag Manager", "Google", TagManager, "loads whatever tracking scripts the site set up there, often Google Analytics and ad pixels"),
    t("doubleclick.net", "Google Marketing Platform (DoubleClick)", "Google", Advertising, "serves and measures ads; its cookie recognises you on every site that uses Google's ad tools"),
    t("googlesyndication.com", "Google AdSense", "Google", Advertising, "shows Google ads on this page, chosen using what Google knows about you"),
    t("googleadservices.com", "Google Ads conversion tracking", "Google", Advertising, "tells Google Ads that you came from one of its ads or did something the advertiser counts"),
    t("adservice.google.com", "Google Ads", "Google", Advertising, "serves and measures Google ads"),
    t("fonts.googleapis.com", "Google Fonts", "Google", Content, "serves this page's fonts; Google sees your IP address and which page asked"),
    t("fonts.gstatic.com", "Google Fonts", "Google", Content, "serves this page's fonts; Google sees your IP address and which page asked"),
    t("youtube.com", "YouTube", "Google", Content, "plays an embedded video; YouTube sees that you visited this page, with your YouTube cookies if you have any"),
    t("youtube-nocookie.com", "YouTube (privacy-enhanced mode)", "Google", Content, "plays an embedded video without setting cookies until you play it"),
    t("bat.bing.com", "Microsoft Advertising (UET)", "Microsoft", Advertising, "tells Microsoft Ads which pages you visit and what you do, to target and measure Bing ads"),
    t("clarity.ms", "Microsoft Clarity", "Microsoft", SessionReplay, "records your clicks, scrolling and mouse movements so the site can replay your visit"),
    t("adnxs.com", "Xandr ad exchange", "Microsoft", Advertising, "runs real-time auctions that share your visit with many advertisers bidding to show you an ad"),
    t("analytics.tiktok.com", "TikTok Pixel", "TikTok (ByteDance)", Advertising, "tells TikTok which pages you visit and what you do, to target and measure TikTok ads"),
    t("ads-twitter.com", "X (Twitter) Pixel", "X Corp.", Advertising, "tells X which pages you visit, to target and measure ads on X"),
    t("analytics.twitter.com", "X (Twitter) Pixel", "X Corp.", Advertising, "tells X which pages you visit, to target and measure ads on X"),
    t("platform.twitter.com", "X (Twitter) embeds", "X Corp.", Social, "shows embedded posts or buttons, and lets X see that you visited this page"),
    t("px.ads.linkedin.com", "LinkedIn Insight Tag", "LinkedIn (Microsoft)", Advertising, "tells LinkedIn which pages you visit, to target and measure ads on LinkedIn"),
    t("snap.licdn.com", "LinkedIn Insight Tag", "LinkedIn (Microsoft)", Advertising, "loads LinkedIn's tracking code, which reports your visit to LinkedIn for ads"),
    t("ct.pinterest.com", "Pinterest Tag", "Pinterest", Advertising, "tells Pinterest which pages you visit, to target and measure ads on Pinterest"),
    t("tr.snapchat.com", "Snap Pixel", "Snap", Advertising, "tells Snapchat which pages you visit, to target and measure Snapchat ads"),
    t("sc-static.net", "Snap Pixel", "Snap", Advertising, "loads Snapchat's tracking code, which reports your visit for ads"),
    t("alb.reddit.com", "Reddit Pixel", "Reddit", Advertising, "tells Reddit which pages you visit, to target and measure ads on Reddit"),
    t("amazon-adsystem.com", "Amazon Ads", "Amazon", Advertising, "serves and measures Amazon ads, using what Amazon knows about you"),
    t("criteo.com", "Criteo", "Criteo", Advertising, "follows the products you look at so their ads can follow you to other sites (retargeting)"),
    t("criteo.net", "Criteo", "Criteo", Advertising, "follows the products you look at so their ads can follow you to other sites (retargeting)"),
    t("taboola.com", "Taboola", "Taboola", Advertising, "shows \"recommended\" sponsored links, chosen from a profile of what you read"),
    t("outbrain.com", "Outbrain", "Outbrain", Advertising, "shows \"recommended\" sponsored links, chosen from a profile of what you read"),
    t("adsrvr.org", "The Trade Desk", "The Trade Desk", Advertising, "buys ads for advertisers, recognising you across sites with its own ID"),
    t("rubiconproject.com", "Magnite (Rubicon Project)", "Magnite", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("pubmatic.com", "PubMatic", "PubMatic", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("openx.net", "OpenX", "OpenX", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("casalemedia.com", "Index Exchange", "Index Exchange", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("rlcdn.com", "LiveRamp", "LiveRamp", Identity, "links your browser to an identity profile that advertisers share"),
    t("id5-sync.com", "ID5", "ID5", Identity, "gives your browser a shared ID that ad companies use to recognise you"),
    t("crwdcntrl.net", "Lotame", "Lotame", Identity, "collects audience data about you to sell to advertisers"),
    t("demdex.net", "Adobe Audience Manager", "Adobe", Identity, "builds an audience profile of you for the site's advertising"),
    t("omtrdc.net", "Adobe Analytics", "Adobe", Analytics, "records which pages you view and what you do on them"),
    t("2o7.net", "Adobe Analytics", "Adobe", Analytics, "records which pages you view and what you do on them"),
    t("scorecardresearch.com", "Comscore", "Comscore", Analytics, "measures audiences across sites for advertisers"),
    t("quantserve.com", "Quantcast", "Quantcast", Advertising, "measures audiences and targets ads across sites"),
    t("quantcount.com", "Quantcast", "Quantcast", Advertising, "measures audiences and targets ads across sites"),
    t("chartbeat.com", "Chartbeat", "Chartbeat", Analytics, "measures what you read and for how long, live"),
    t("chartbeat.net", "Chartbeat", "Chartbeat", Analytics, "measures what you read and for how long, live"),
    t("mc.yandex.ru", "Yandex Metrica", "Yandex", SessionReplay, "records your visit, which it can replay, including clicks and scrolling"),
    t("hotjar.com", "Hotjar", "Hotjar (Contentsquare)", SessionReplay, "records your clicks, scrolling and typing so the site can replay your visit"),
    t("hotjar.io", "Hotjar", "Hotjar (Contentsquare)", SessionReplay, "records your clicks, scrolling and typing so the site can replay your visit"),
    t("fullstory.com", "FullStory", "FullStory", SessionReplay, "records everything you do on the page so the site can replay your visit"),
    t("mouseflow.com", "Mouseflow", "Mouseflow", SessionReplay, "records your mouse movements, clicks and scrolling so the site can replay your visit"),
    t("lr-ingest.io", "LogRocket", "LogRocket", SessionReplay, "records your visit so the site can replay it"),
    t("segment.io", "Segment", "Twilio", Analytics, "collects what you do and passes it on to the site's other analytics and ad tools"),
    t("segment.com", "Segment", "Twilio", Analytics, "collects what you do and passes it on to the site's other analytics and ad tools"),
    t("mixpanel.com", "Mixpanel", "Mixpanel", Analytics, "records what you click and do, under an ID for you"),
    t("amplitude.com", "Amplitude", "Amplitude", Analytics, "records what you click and do, under an ID for you"),
    t("heapanalytics.com", "Heap", "Contentsquare", Analytics, "records every click and page view, under an ID for you"),
    t("pendo.io", "Pendo", "Pendo", Analytics, "records how you use the site, under an ID for you"),
    t("hs-analytics.net", "HubSpot", "HubSpot", Analytics, "tracks your visits for the site's marketing, and can tie them to your email address"),
    t("optimizely.com", "Optimizely", "Optimizely", Analytics, "puts you in A/B tests and measures what you do"),
    t("nr-data.net", "New Relic", "New Relic", Monitoring, "measures how fast the page loads for you and reports errors"),
    t("sentry.io", "Sentry", "Sentry", Monitoring, "reports errors in the page's code, with details of your browser"),
    t("browser-intake-datadoghq.com", "Datadog", "Datadog", Monitoring, "measures the page's speed and errors, and can record sessions"),
    t("cloudflareinsights.com", "Cloudflare Web Analytics", "Cloudflare", Analytics, "counts visits and page speed, without cookies"),
    t("plausible.io", "Plausible Analytics", "Plausible", Analytics, "counts visits without cookies or a personal ID"),
    t("fpjs.io", "Fingerprint", "Fingerprint", Fingerprinting, "identifies your browser from its characteristics, so it can recognise you without cookies"),
    t("cookielaw.org", "OneTrust", "OneTrust", Consent, "shows the cookie banner and remembers what you chose"),
    t("onetrust.com", "OneTrust", "OneTrust", Consent, "shows the cookie banner and remembers what you chose"),
    t("cookiebot.com", "Cookiebot", "Usercentrics", Consent, "shows the cookie banner and remembers what you chose"),
    t("disqus.com", "Disqus", "Disqus", Social, "shows comments, and lets Disqus see that you visited this page"),
    t("sharethis.com", "ShareThis", "ShareThis", Social, "shows share buttons, and collects data about your visit"),
    t("branch.io", "Branch", "Branch", Advertising, "tracks which link or ad brought you here, across apps and sites"),
    t("appsflyer.com", "AppsFlyer", "AppsFlyer", Advertising, "tracks which ad brought you here, for the advertiser"),
];

/// Cookies whose purpose is publicly documented by their makers.
const COOKIES: &[(&str, &str)] = &[
    ("_ga", "Google Analytics' ID for you"),
    ("_gid", "Google Analytics' ID for you (24 hours)"),
    ("_gcl_au", "Google Ads' conversion ID"),
    ("IDE", "Google's ad ID for you"),
    ("NID", "Google's preferences and ad ID"),
    ("_fbp", "Meta Pixel's ID for you"),
    ("_fbc", "the Facebook ad you clicked"),
    ("fr", "Meta's ad ID"),
    ("datr", "Meta's ID for this browser"),
    ("MUID", "Microsoft's ID for you"),
    ("_uetsid", "Microsoft Ads' ID for you"),
    ("_uetvid", "Microsoft Ads' ID for you"),
    ("_ttp", "TikTok Pixel's ID for you"),
    ("bcookie", "LinkedIn's ID for this browser"),
    ("li_sugr", "LinkedIn's ad ID"),
    ("_hjSessionUser", "Hotjar's ID for you"),
    ("_clck", "Microsoft Clarity's ID for you"),
    ("uuid2", "Xandr's ad ID for you"),
    ("demdex", "Adobe's ID for you"),
];

/// Click and campaign parameters, which Brickle strips from links.
const PARAMS: &[(&str, &str)] = &[
    ("fbclid", "which Facebook post or ad you clicked"),
    ("gclid", "which Google ad you clicked"),
    ("gbraid", "which Google ad you clicked"),
    ("wbraid", "which Google ad you clicked"),
    ("dclid", "which Google display ad you clicked"),
    ("msclkid", "which Microsoft ad you clicked"),
    ("ttclid", "which TikTok ad you clicked"),
    ("twclid", "which X (Twitter) ad you clicked"),
    ("li_fat_id", "which LinkedIn ad you clicked"),
    ("igshid", "who shared the Instagram link"),
    ("yclid", "which Yandex ad you clicked"),
    ("mc_eid", "your address in a Mailchimp mailing list"),
    ("_hsenc", "you as a recipient of a HubSpot email"),
    ("utm_", "which campaign, email or link brought you here"),
];

fn host_of(url: &str) -> String {
    crate::filter::host_of(url)
}

fn matches_at(url: &str, at: &str) -> bool {
    let (want_host, want_path) = match at.find('/') {
        Some(i) => (&at[..i], Some(&at[i..])),
        None => (at, None),
    };
    let host = host_of(url);
    let host_ok = host == want_host || host.ends_with(&format!(".{want_host}"));
    host_ok
        && want_path.is_none_or(|p| {
            url::Url::parse(url).is_ok_and(|u| u.path() == p || u.path().starts_with(&format!("{p}/")))
        })
}

/// The tracker `url` goes to, if it's one Brickle knows.
pub fn tracker(url: &str) -> Option<&'static Tracker> {
    TRACKERS.iter().find(|t| matches_at(url, t.at))
}

/// Does this value look like an identifier (long, random-looking)?
fn id_like(v: &str) -> bool {
    v.len() >= 16
        && v.len() <= 512
        && v.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_ascii_alphabetic())
}

/// Plain-language lines about request `e`, made by the page at
/// `page_url`. Empty for the page's own requests that carry nothing worth
/// pointing out.
pub fn explain(e: &LogEntry, page_url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let third_party = crate::cookies::site(&e.url) != crate::cookies::site(page_url);
    let blocked = matches!(e.outcome, Outcome::Blocked { .. } | Outcome::Denied { .. });
    let sent = e.was_sent();

    if let Some(t) = tracker(&e.url).filter(|_| third_party) {
        out.push(format!(
            "{} ({}) · {}: it {}",
            t.name,
            t.company,
            t.kind.label(),
            t.what
        ));
    }

    // What it carries (or would have, had it not been blocked).
    let mut carries = Vec::new();
    let query: Vec<(String, String)> = url::Url::parse(&e.url)
        .map(|u| {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .take(200)
                .collect()
        })
        .unwrap_or_default();
    let page_host = host_of(page_url);
    if third_party && !page_host.is_empty() {
        if let Some((k, _)) = query.iter().find(|(_, v)| v.contains(&page_host)) {
            carries.push(format!("this page's address (in `{k}`)"));
        }
    }
    if third_party {
        let ids: Vec<&str> = query
            .iter()
            .filter(|(_, v)| id_like(v))
            .map(|(k, _)| k.as_str())
            .take(4)
            .collect();
        if !ids.is_empty() {
            let list = ids
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", ");
            carries.push(format!("ID-like values in {list}"));
        }
    }
    let known = third_party && tracker(&e.url).is_some();
    if sent && known && !e.cookies_sent.is_empty() {
        carries.push(format!(
            "your cookies for {}, which let it recognise you on every site that uses it",
            crate::cookies::site(&e.url)
        ));
    }
    if !carries.is_empty() {
        let verb = if blocked { "Would have sent" } else { "Sent" };
        out.push(format!("{verb}: {}", carries.join("; ")));
    }
    let mut named: Vec<String> = Vec::new();
    for c in e.cookies_sent.iter().chain(&e.cookies_set) {
        let known = COOKIES
            .iter()
            .find(|(n, _)| c == n || (n.len() > 4 && c.starts_with(n)));
        if let Some((name, what)) = known {
            let line = format!("{name} is {what}");
            if !named.contains(&line) {
                named.push(line);
            }
        }
    }
    if !named.is_empty() {
        out.push(format!("Cookies: {}", named.join("; ")));
    }

    // A tiny image with lots of data in its address is a tracking pixel.
    if third_party && e.dest == crate::net::Dest::Image && query.len() >= 5 {
        out.push("An image request carrying data in its address: a tracking pixel, not a picture".into());
    }

    for p in &e.stripped {
        let name = p.split('=').next().unwrap_or(p);
        if let Some((_, what)) = PARAMS
            .iter()
            .find(|(k, _)| name.eq_ignore_ascii_case(k) || (k.ends_with('_') && name.starts_with(k)))
        {
            out.push(format!(
                "Removed `{name}` from the address: it tells the site {what}"
            ));
        }
    }

    match &e.outcome {
        // (The rule itself is in the request's notes.)
        Outcome::Blocked { .. } => {
            if let Some(list) = &e.list {
                out.push(format!("Blocked by the {list} list"));
            }
        }
        Outcome::CrossSite { .. } => out.push(
            "A script on this page tried to send this to another site; Brickle keeps scripts to their own site".into(),
        ),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Dest;

    fn entry(url: &str, dest: Dest, outcome: Outcome) -> LogEntry {
        LogEntry {
            url: url.into(),
            dest,
            outcome,
            stripped: vec![],
            upgraded: false,
            insecure: false,
            allowed_by: None,
            cookies_sent: vec![],
            cookies_set: vec![],
            cookies_refused: vec![],
            cors_blocked: false,
            list: None,
        }
    }

    #[test]
    fn a_blocked_pixel_is_explained() {
        let mut e = entry(
            "https://www.facebook.com/tr?id=123456&ev=PageView&dl=https%3A%2F%2Fshop.example%2Fcart&fbp=fb.1.1699999999.1234567890abc&ts=1&v=2",
            Dest::Image,
            Outcome::Blocked {
                rule: "||facebook.com/tr^".into(),
            },
        );
        e.list = Some("EasyPrivacy (trackers)".into());
        let lines = explain(&e, "https://shop.example/cart");
        let all = lines.join("\n");
        assert!(
            lines[0].starts_with("Meta Pixel (Meta) · advertising: it tells Meta"),
            "{all}"
        );
        assert!(
            all.contains("Would have sent: this page's address (in `dl`)"),
            "{all}"
        );
        assert!(all.contains("`fbp`"), "{all}");
        assert!(all.contains("tracking pixel"), "{all}");
        assert!(
            all.contains("Blocked by the EasyPrivacy (trackers) list"),
            "{all}"
        );
    }

    #[test]
    fn first_party_requests_say_nothing_much() {
        // facebook.com on facebook.com isn't "Meta Pixel".
        let mut e = entry(
            "https://www.facebook.com/?x=1",
            Dest::Document,
            Outcome::Loaded { status: 200 },
        );
        e.cookies_sent = vec!["datr".into(), "fr".into()];
        let lines = explain(&e, "https://www.facebook.com/");
        assert!(!lines.join(" ").contains("Meta Pixel"));
        assert!(!lines.join(" ").contains("every site"));
        // But its cookies are still named.
        assert!(lines.join(" ").contains("fr is Meta's ad ID"), "{lines:?}");
        let plain = entry(
            "https://site.example/a.css",
            Dest::Stylesheet,
            Outcome::Loaded { status: 200 },
        );
        assert!(explain(&plain, "https://site.example/").is_empty());
    }

    #[test]
    fn cookies_params_and_paths() {
        let mut e = entry(
            "https://www.google-analytics.com/g/collect?v=2&tid=G-ABC",
            Dest::Xhr,
            Outcome::Loaded { status: 204 },
        );
        e.cookies_sent = vec!["_ga_ABC123".into()];
        let all = explain(&e, "https://news.example/").join("\n");
        assert!(all.contains("Google Analytics (Google) · analytics"), "{all}");
        assert!(
            all.contains("Sent: your cookies for google-analytics.com"),
            "{all}"
        );
        let mut nav = entry(
            "https://shop.example/",
            Dest::Document,
            Outcome::Loaded { status: 200 },
        );
        nav.stripped = vec!["fbclid".into(), "utm_source".into()];
        let all = explain(&nav, "https://shop.example/").join("\n");
        assert!(all.contains("`fbclid`") && all.contains("`utm_source`"), "{all}");
        // Paths: /tr is the pixel, /trash isn't.
        assert!(tracker("https://facebook.com/tr/").is_some());
        assert!(tracker("https://facebook.com/trash").is_none());
        assert!(tracker("https://notdoubleclick.net/").is_none());
        assert!(tracker("https://ad.doubleclick.net/x").is_some());
    }
}
