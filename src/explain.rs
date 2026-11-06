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
    BotDetection,
    Affiliate,
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
            Kind::BotDetection => "bot detection",
            Kind::Affiliate => "affiliate marketing",
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
    t("google.com/recaptcha", "reCAPTCHA", "Google", BotDetection, "checks you're not a bot by examining your browser and how you move, click and type"),
    t("gstatic.com/recaptcha", "reCAPTCHA", "Google", BotDetection, "checks you're not a bot by examining your browser and how you move, click and type"),
    t("google.com/pagead", "Google Ads conversion tracking", "Google", Advertising, "tells Google Ads that you came from one of its ads or did something the advertiser counts"),
    t("google.com/ads", "Google Analytics advertising features", "Google", Advertising, "adds you to ad audiences based on this visit, so the site's ads can follow you"),
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
    // More advertising.
    t("googletagservices.com", "Google Publisher Tag", "Google", Advertising, "loads Google's ad-serving code, which picks ads using what Google knows about you"),
    t("2mdn.net", "Google ad content (DoubleClick)", "Google", Advertising, "serves the images and videos of ads from Google's ad platform"),
    t("adnxs-simple.com", "Xandr ad exchange", "Microsoft", Advertising, "runs real-time auctions that share your visit with many advertisers bidding to show you an ad"),
    t("indexww.com", "Index Exchange", "Index Exchange", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("ads.yahoo.com", "Yahoo Ads", "Yahoo", Advertising, "serves and measures Yahoo ads, recognising you across sites"),
    t("analytics.yahoo.com", "Yahoo Dot tag", "Yahoo", Advertising, "tells Yahoo which pages you visit, to target and measure its ads"),
    t("advertising.com", "Yahoo Ads", "Yahoo", Advertising, "serves and measures Yahoo ads, recognising you across sites"),
    t("an.yandex.ru", "Yandex Advertising Network", "Yandex", Advertising, "serves Yandex ads, chosen using what Yandex knows about you"),
    t("3lift.com", "TripleLift", "TripleLift", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("triplelift.com", "TripleLift", "TripleLift", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("sharethrough.com", "Sharethrough", "Sharethrough", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("yieldmo.com", "Yieldmo", "Yieldmo", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("33across.com", "33Across", "33Across", Advertising, "runs ad auctions and gives your browser an ID that advertisers share"),
    t("smartadserver.com", "Equativ (Smart AdServer)", "Equativ", Advertising, "serves ads and runs auctions that share your visit with advertisers"),
    t("teads.tv", "Teads", "Teads", Advertising, "serves video ads inside articles, and measures whether you watched them"),
    t("media.net", "Media.net", "Media.net", Advertising, "serves contextual ads and runs auctions that share your visit with advertisers"),
    t("contextweb.com", "PulsePoint", "PulsePoint", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("lijit.com", "Sovrn", "Sovrn", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("sovrn.com", "Sovrn", "Sovrn", Advertising, "runs ad auctions that share your visit with advertisers"),
    t("adform.net", "Adform", "Adform", Advertising, "serves and measures ads, recognising you across sites"),
    t("bidswitch.net", "BidSwitch", "IPONWEB (Criteo)", Advertising, "passes your visit between ad exchanges and the advertisers bidding on it"),
    t("mathtag.com", "MediaMath", "MediaMath", Advertising, "buys ads for advertisers, recognising you across sites with its own ID"),
    t("everesttech.net", "Adobe Advertising", "Adobe", Advertising, "buys and measures ads for advertisers, recognising you across sites"),
    t("serving-sys.com", "Amazon Ad Server (Sizmek)", "Amazon", Advertising, "serves and measures ads for advertisers"),
    t("flashtalking.com", "Flashtalking", "Mediaocean", Advertising, "serves and measures ads for advertisers"),
    t("adroll.com", "AdRoll", "NextRoll", Advertising, "follows the pages you look at so ads for this site can follow you to other sites (retargeting)"),
    t("rfihub.com", "Zeta Global", "Zeta Global", Advertising, "buys ads for advertisers, recognising you across sites with its own ID"),
    t("revcontent.com", "Revcontent", "Revcontent", Advertising, "shows \"recommended\" sponsored links, chosen from a profile of what you read"),
    t("mgid.com", "MGID", "MGID", Advertising, "shows \"recommended\" sponsored links, chosen from a profile of what you read"),
    t("bounceexchange.com", "Wunderkind", "Wunderkind", Advertising, "tracks what you do on the site to show you pop-ups and send you marketing emails"),
    t("bouncex.net", "Wunderkind", "Wunderkind", Advertising, "tracks what you do on the site to show you pop-ups and send you marketing emails"),
    t("q.quora.com", "Quora Pixel", "Quora", Advertising, "tells Quora which pages you visit, to target and measure ads on Quora"),
    t("pixels.spotify.com", "Spotify Pixel", "Spotify", Advertising, "tells Spotify which pages you visit, to measure its ads"),
    t("vk.com/rtrg", "VK Pixel", "VK", Advertising, "tells VK which pages you visit, to target ads on VK"),
    t("adjust.com", "Adjust", "Adjust (AppLovin)", Advertising, "tracks which ad brought you here, for the advertiser"),
    t("kochava.com", "Kochava", "Kochava", Advertising, "tracks which ad brought you here, for the advertiser"),
    t("singular.net", "Singular", "Singular", Advertising, "tracks which ad brought you here, for the advertiser"),
    // Ad verification.
    t("moatads.com", "Moat", "Oracle", Advertising, "measures whether you saw the ads on this page, and whether you're a real person"),
    t("doubleverify.com", "DoubleVerify", "DoubleVerify", Advertising, "measures whether you saw the ads on this page, and whether you're a real person"),
    t("adsafeprotected.com", "Integral Ad Science", "Integral Ad Science", Advertising, "measures whether you saw the ads on this page, and whether you're a real person"),
    // Data brokers and shared IDs.
    t("bluekai.com", "Oracle BlueKai", "Oracle", Identity, "collects data about your browsing to sell audience profiles to advertisers"),
    t("agkn.com", "Neustar (TransUnion)", "TransUnion", Identity, "links your browser to an identity profile that advertisers buy"),
    t("tapad.com", "Tapad", "Experian", Identity, "links your browser to your other devices, so advertisers can recognise you on all of them"),
    t("krxd.net", "Salesforce Audience Studio (Krux)", "Salesforce", Identity, "builds an audience profile of you for the site's advertising"),
    t("exelator.com", "Nielsen eXelate", "Nielsen", Identity, "collects data about your browsing to sell audience profiles to advertisers"),
    t("liadm.com", "LiveIntent", "LiveIntent", Identity, "gives your browser an ID tied to your email address, which advertisers share"),
    t("pippio.com", "LiveRamp", "LiveRamp", Identity, "links your browser to an identity profile that advertisers share"),
    t("adsymptotic.com", "Drawbridge", "LinkedIn (Microsoft)", Identity, "links your browser to your other devices, so advertisers can recognise you on all of them"),
    t("zeotap.com", "Zeotap", "Zeotap", Identity, "links your browser to an identity profile that advertisers share"),
    t("permutive.com", "Permutive", "Permutive", Identity, "builds an audience profile of you in your browser for the site's ads"),
    t("permutive.app", "Permutive", "Permutive", Identity, "builds an audience profile of you in your browser for the site's ads"),
    // Affiliate links.
    t("awin1.com", "Awin", "Awin", Affiliate, "records which affiliate link brought you here, so its owner is paid commission"),
    t("anrdoezrs.net", "CJ Affiliate", "Publicis", Affiliate, "records which affiliate link brought you here, so its owner is paid commission"),
    t("linksynergy.com", "Rakuten Advertising", "Rakuten", Affiliate, "records which affiliate link brought you here, so its owner is paid commission"),
    t("shareasale.com", "ShareASale", "Awin", Affiliate, "records which affiliate link brought you here, so its owner is paid commission"),
    t("skimresources.com", "Skimlinks", "Connexity", Affiliate, "turns ordinary links on the page into affiliate links, and records what you click"),
    t("viglink.com", "Sovrn Commerce (VigLink)", "Sovrn", Affiliate, "turns ordinary links on the page into affiliate links, and records what you click"),
    // More analytics.
    t("imrworldwide.com", "Nielsen Digital Measurement", "Nielsen", Analytics, "measures audiences across sites for advertisers"),
    t("gemius.pl", "Gemius", "Gemius", Analytics, "measures audiences across sites for advertisers"),
    t("hm.baidu.com", "Baidu Tongji", "Baidu", Analytics, "records which pages you view and where you came from"),
    t("cnzz.com", "Umeng (CNZZ)", "Alibaba", Analytics, "records which pages you view and where you came from"),
    t("top-fwz1.mail.ru", "Top.Mail.Ru", "VK", Analytics, "counts your visits and measures audiences for advertisers"),
    t("mc.yandex.com", "Yandex Metrica", "Yandex", SessionReplay, "records your visit, which it can replay, including clicks and scrolling"),
    t("stats.wp.com", "WordPress.com Stats (Jetpack)", "Automattic", Analytics, "counts your visit for the site's WordPress.com statistics"),
    t("pixel.wp.com", "WordPress.com Stats (Jetpack)", "Automattic", Analytics, "counts your visit for the site's WordPress.com statistics"),
    t("parsely.com", "Parse.ly", "Automattic", Analytics, "measures what you read and for how long"),
    t("parse.ly", "Parse.ly", "Automattic", Analytics, "measures what you read and for how long"),
    t("statcounter.com", "StatCounter", "StatCounter", Analytics, "records which pages you view and where you came from"),
    t("getclicky.com", "Clicky", "Clicky", Analytics, "records which pages you view, under an ID for you"),
    t("kissmetrics.com", "Kissmetrics", "Kissmetrics", Analytics, "records what you do, under an ID for you"),
    t("matomo.cloud", "Matomo Cloud", "InnoCraft", Analytics, "records your visit for the site's own analytics"),
    t("googleoptimize.com", "Google Optimize", "Google", Analytics, "puts you in A/B tests and measures what you do"),
    t("visualwebsiteoptimizer.com", "VWO", "Wingify", Analytics, "puts you in A/B tests and can record your visit"),
    t("vwo.com", "VWO", "Wingify", Analytics, "puts you in A/B tests and can record your visit"),
    t("abtasty.com", "AB Tasty", "AB Tasty", Analytics, "puts you in A/B tests and measures what you do"),
    t("launchdarkly.com", "LaunchDarkly", "LaunchDarkly", Analytics, "turns site features on or off for you, and reports which you saw"),
    t("posthog.com", "PostHog", "PostHog", Analytics, "records what you click and do, and can record your visit to replay it"),
    t("rudderstack.com", "RudderStack", "RudderStack", Analytics, "collects what you do and passes it on to the site's other analytics and ad tools"),
    t("mparticle.com", "mParticle", "Rokt", Analytics, "collects what you do and passes it on to the site's other analytics and ad tools"),
    t("heap-api.com", "Heap", "Contentsquare", Analytics, "records every click and page view, under an ID for you"),
    t("qualtrics.com", "Qualtrics", "Qualtrics", Analytics, "shows surveys and records how you use the site"),
    t("piano.io", "Piano", "Piano", Analytics, "measures what you read, and decides whether to show you a paywall"),
    t("tinypass.com", "Piano", "Piano", Analytics, "measures what you read, and decides whether to show you a paywall"),
    t("cxense.com", "Piano (Cxense)", "Piano", Analytics, "builds a profile of what you read, for recommendations and ads"),
    t("simpleanalyticscdn.com", "Simple Analytics", "Simple Analytics", Analytics, "counts visits without cookies or a personal ID"),
    t("usefathom.com", "Fathom Analytics", "Fathom", Analytics, "counts visits without cookies or a personal ID"),
    t("goatcounter.com", "GoatCounter", "GoatCounter", Analytics, "counts visits without cookies or a personal ID"),
    // Marketing automation: visits tied to your email address.
    t("hs-scripts.com", "HubSpot", "HubSpot", Analytics, "loads HubSpot's tracking code, which records your visits and can tie them to your email address"),
    t("hscollectedforms.net", "HubSpot forms", "HubSpot", Analytics, "collects what you type into the site's forms for its marketing"),
    t("hsforms.net", "HubSpot forms", "HubSpot", Analytics, "shows a form and records your visit for the site's marketing"),
    t("marketo.net", "Marketo", "Adobe", Analytics, "records your visits for the site's marketing, tied to your email address once you fill in a form"),
    t("mktoresp.com", "Marketo", "Adobe", Analytics, "records your visits for the site's marketing, tied to your email address once you fill in a form"),
    t("pardot.com", "Salesforce Pardot", "Salesforce", Analytics, "records your visits for the site's marketing, tied to your email address once you fill in a form"),
    t("eloqua.com", "Oracle Eloqua", "Oracle", Analytics, "records your visits for the site's marketing, tied to your email address once you fill in a form"),
    t("exacttarget.com", "Salesforce Marketing Cloud", "Salesforce", Analytics, "records what you look at for the site's marketing emails"),
    t("igodigital.com", "Salesforce Marketing Cloud", "Salesforce", Analytics, "records what you look at for the site's marketing emails"),
    t("klaviyo.com", "Klaviyo", "Klaviyo", Analytics, "records what you look at for the site's marketing emails and texts"),
    t("list-manage.com", "Mailchimp", "Intuit", Analytics, "handles mailing-list sign-ups and tracks visits from the site's emails"),
    t("chimpstatic.com", "Mailchimp", "Intuit", Analytics, "loads Mailchimp's code for sign-up forms and tracking visits from emails"),
    // Session recording.
    t("crazyegg.com", "Crazy Egg", "Crazy Egg", SessionReplay, "records your clicks and scrolling for heatmaps and replays"),
    t("luckyorange.com", "Lucky Orange", "Lucky Orange", SessionReplay, "records your visit so the site can replay it"),
    t("luckyorange.net", "Lucky Orange", "Lucky Orange", SessionReplay, "records your visit so the site can replay it"),
    t("smartlook.com", "Smartlook", "Cisco", SessionReplay, "records your visit so the site can replay it"),
    t("smartlook.cloud", "Smartlook", "Cisco", SessionReplay, "records your visit so the site can replay it"),
    t("inspectlet.com", "Inspectlet", "Inspectlet", SessionReplay, "records your visit, including what you type, so the site can replay it"),
    t("contentsquare.net", "Contentsquare", "Contentsquare", SessionReplay, "records your clicks, scrolling and mouse movements so the site can replay your visit"),
    t("quantummetric.com", "Quantum Metric", "Quantum Metric", SessionReplay, "records your visit so the site can replay it"),
    // Monitoring.
    t("js-agent.newrelic.com", "New Relic", "New Relic", Monitoring, "measures how fast the page loads for you and reports errors"),
    t("sentry-cdn.com", "Sentry", "Sentry", Monitoring, "reports errors in the page's code, with details of your browser"),
    t("datadoghq-browser-agent.com", "Datadog", "Datadog", Monitoring, "measures the page's speed and errors, and can record sessions"),
    t("bugsnag.com", "Bugsnag", "SmartBear", Monitoring, "reports errors in the page's code, with details of your browser"),
    t("rollbar.com", "Rollbar", "Rollbar", Monitoring, "reports errors in the page's code, with details of your browser"),
    t("raygun.io", "Raygun", "Raygun", Monitoring, "reports errors and page speed, with details of your browser"),
    t("akstat.io", "Akamai mPulse", "Akamai", Monitoring, "measures how fast the page loads for you"),
    t("speedcurve.com", "SpeedCurve", "SpeedCurve", Monitoring, "measures how fast the page loads for you"),
    // Tag managers.
    t("tiqcdn.com", "Tealium", "Tealium", TagManager, "loads whatever tracking scripts the site set up there"),
    t("tealiumiq.com", "Tealium", "Tealium", TagManager, "collects what you do and passes it on to the site's analytics and ad tools"),
    t("ensighten.com", "Ensighten", "Ensighten", TagManager, "loads whatever tracking scripts the site set up there"),
    t("adobedtm.com", "Adobe Experience Platform tags", "Adobe", TagManager, "loads whatever tracking scripts the site set up there, often Adobe Analytics"),
    // Bot detection and fraud checks (fingerprinting).
    t("recaptcha.net", "reCAPTCHA", "Google", BotDetection, "checks you're not a bot by examining your browser and how you move, click and type"),
    t("hcaptcha.com", "hCaptcha", "Intuition Machines", BotDetection, "checks you're not a bot by examining your browser and asking you to pick pictures"),
    t("challenges.cloudflare.com", "Cloudflare Turnstile", "Cloudflare", BotDetection, "checks you're not a bot by examining your browser"),
    t("perimeterx.net", "HUMAN (PerimeterX)", "HUMAN", BotDetection, "checks you're not a bot from your browser's details and behaviour"),
    t("px-cdn.net", "HUMAN (PerimeterX)", "HUMAN", BotDetection, "checks you're not a bot from your browser's details and behaviour"),
    t("px-cloud.net", "HUMAN (PerimeterX)", "HUMAN", BotDetection, "checks you're not a bot from your browser's details and behaviour"),
    t("datadome.co", "DataDome", "DataDome", BotDetection, "checks you're not a bot from your browser's details and behaviour"),
    t("arkoselabs.com", "Arkose Labs", "Arkose Labs", BotDetection, "checks you're not a bot, with puzzles and your browser's details"),
    t("online-metrix.net", "ThreatMetrix", "LexisNexis Risk Solutions", Fingerprinting, "identifies your device from many details of your browser, for fraud checks shared between companies"),
    t("iovation.com", "iovation", "TransUnion", Fingerprinting, "identifies your device from details of your browser, for fraud checks shared between companies"),
    t("sift.com", "Sift", "Sift", Fingerprinting, "identifies your device and watches what you do, for fraud checks"),
    t("siftscience.com", "Sift", "Sift", Fingerprinting, "identifies your device and watches what you do, for fraud checks"),
    t("fpcdn.io", "Fingerprint", "Fingerprint", Fingerprinting, "identifies your browser from its characteristics, so it can recognise you without cookies"),
    t("fpjscdn.net", "Fingerprint", "Fingerprint", Fingerprinting, "identifies your browser from its characteristics, so it can recognise you without cookies"),
    t("fingerprintjs.com", "Fingerprint", "Fingerprint", Fingerprinting, "identifies your browser from its characteristics, so it can recognise you without cookies"),
    // Social and embeds.
    t("addthis.com", "AddThis", "Oracle", Social, "shows share buttons, and collects data about your visit for advertisers"),
    t("addtoany.com", "AddToAny", "AddToAny", Social, "shows share buttons"),
    t("instagram.com", "Instagram", "Meta", Social, "shows an embedded post, and lets Meta see that you visited this page"),
    t("platform.linkedin.com", "LinkedIn embeds", "LinkedIn (Microsoft)", Social, "shows LinkedIn buttons or posts, and lets LinkedIn see that you visited this page"),
    t("tiktok.com", "TikTok", "TikTok (ByteDance)", Social, "shows an embedded video, and lets TikTok see that you visited this page"),
    t("player.vimeo.com", "Vimeo", "Vimeo", Content, "plays an embedded video; Vimeo sees that you visited this page"),
    t("open.spotify.com", "Spotify", "Spotify", Content, "plays embedded music; Spotify sees that you visited this page"),
    t("w.soundcloud.com", "SoundCloud", "SoundCloud", Content, "plays embedded audio; SoundCloud sees that you visited this page"),
    t("maps.googleapis.com", "Google Maps", "Google", Content, "shows a map; Google sees your IP address and which page asked"),
    t("maps.google.com", "Google Maps", "Google", Content, "shows a map; Google sees your IP address and which page asked"),
    t("gravatar.com", "Gravatar", "Automattic", Content, "shows profile pictures, found by a code made from each person's email address"),
    t("ajax.googleapis.com", "Google Hosted Libraries", "Google", Content, "serves code libraries; Google sees your IP address and when you visited"),
    t("cdnjs.cloudflare.com", "cdnjs", "Cloudflare", Content, "serves code libraries; Cloudflare sees your IP address and when you visited"),
    t("cdn.jsdelivr.net", "jsDelivr", "jsDelivr", Content, "serves code libraries; it sees your IP address and when you visited"),
    t("unpkg.com", "unpkg", "unpkg", Content, "serves code libraries; it sees your IP address and when you visited"),
    t("use.typekit.net", "Adobe Fonts", "Adobe", Content, "serves this page's fonts and counts page views for the font licence"),
    t("p.typekit.net", "Adobe Fonts", "Adobe", Content, "counts page views for the font licence"),
    t("kit.fontawesome.com", "Font Awesome", "Fonticons", Content, "serves the page's icons; it sees your IP address and which site asked"),
    // Chat and support widgets.
    t("intercom.io", "Intercom", "Intercom", Content, "shows a chat window, and records your visits for the site's support and marketing"),
    t("intercomcdn.com", "Intercom", "Intercom", Content, "shows a chat window, and records your visits for the site's support and marketing"),
    t("drift.com", "Drift", "Salesloft", Content, "shows a chat window, and records your visits for the site's sales team"),
    t("zdassets.com", "Zendesk", "Zendesk", Content, "shows a help or chat window"),
    t("tawk.to", "tawk.to", "tawk.to", Content, "shows a chat window, and records your visits for the site"),
    t("crisp.chat", "Crisp", "Crisp", Content, "shows a chat window, and records your visits for the site"),
    t("onesignal.com", "OneSignal", "OneSignal", Content, "asks to send you notifications, and tracks whether you opened them"),
    // Cookie banners.
    t("consensu.org", "IAB consent framework", "IAB Europe", Consent, "shows the cookie banner and passes what you chose on to the site's ad partners"),
    t("usercentrics.eu", "Usercentrics", "Usercentrics", Consent, "shows the cookie banner and remembers what you chose"),
    t("trustarc.com", "TrustArc", "TrustArc", Consent, "shows the cookie banner and remembers what you chose"),
    t("didomi.io", "Didomi", "Didomi", Consent, "shows the cookie banner and remembers what you chose"),
    t("privacy-mgmt.com", "Sourcepoint", "Sourcepoint", Consent, "shows the cookie banner and remembers what you chose"),
    t("cookieyes.com", "CookieYes", "CookieYes", Consent, "shows the cookie banner and remembers what you chose"),
    t("iubenda.com", "iubenda", "iubenda", Consent, "shows the cookie banner and remembers what you chose"),
    t("termly.io", "Termly", "Termly", Consent, "shows the cookie banner and remembers what you chose"),
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
    ("_gcl_aw", "the Google ad you clicked"),
    ("__gads", "Google AdSense's ID for you"),
    ("__gpi", "Google AdSense's ID for you"),
    (
        "test_cookie",
        "Google's check that cookies work, before its ad ID",
    ),
    ("ANID", "Google's ad ID"),
    ("_pin_unauth", "Pinterest Tag's ID for you"),
    ("_scid", "Snap Pixel's ID for you"),
    ("_rdt_uuid", "Reddit Pixel's ID for you"),
    ("personalization_id", "X's ad ID for you"),
    ("li_gc", "LinkedIn's record of your cookie choice"),
    ("lidc", "LinkedIn's server choice"),
    ("AnalyticsSyncHistory", "LinkedIn's ad ID syncing"),
    ("_hjid", "Hotjar's ID for you"),
    ("_clsk", "Microsoft Clarity's session"),
    ("ajs_anonymous_id", "Segment's ID for you"),
    ("ajs_user_id", "Segment's ID for you, once you're signed in"),
    ("mp_", "Mixpanel's ID for you"),
    ("amp_", "Amplitude's ID for you"),
    ("_hjFirstSeen", "Hotjar's note that this is your first visit"),
    (
        "hubspotutk",
        "HubSpot's ID for you, tied to your email once you fill in a form",
    ),
    ("__hstc", "HubSpot's record of your visits"),
    (
        "_mkto_trk",
        "Marketo's ID for you, tied to your email once you fill in a form",
    ),
    ("_ym_uid", "Yandex Metrica's ID for you"),
    ("yandexuid", "Yandex's ID for you"),
    ("tuuid", "an ad exchange's ID for you"),
    ("TDID", "The Trade Desk's ID for you"),
    ("TapAd_DID", "Tapad's cross-device ID for you"),
    ("bku", "Oracle BlueKai's ID for you"),
    ("IDSYNC", "Yahoo's ad ID syncing"),
    ("criteo_write_test", "Criteo's check that it can set cookies"),
    ("cto_bundle", "Criteo's ID for you"),
    ("__qca", "Quantcast's ID for you"),
    ("OptanonConsent", "your choices in the OneTrust cookie banner"),
    ("CookieConsent", "your choices in the Cookiebot cookie banner"),
    (
        "euconsent-v2",
        "your cookie choices, shared with the site's ad partners",
    ),
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
    ("_hsmi", "which HubSpot email you clicked"),
    ("mkt_tok", "you as a recipient of a Marketo email"),
    ("oly_enc_id", "you as a recipient of an Omeda email"),
    ("vero_id", "you as a recipient of a Vero email"),
    ("_kx", "you as a recipient of a Klaviyo email"),
    ("s_cid", "which Adobe campaign brought you here"),
    ("epik", "which Pinterest ad you clicked"),
    ("sccid", "which Snapchat ad you clicked"),
    ("rdt_cid", "which Reddit ad you clicked"),
    ("irclickid", "which affiliate link you clicked (Impact)"),
    ("utm_", "which campaign, email or link brought you here"),
];

/// Details of the browser that fingerprinting scripts collect, as the
/// script runtime reports reading them, with what each tells.
pub const FINGERPRINT: &[(&str, &str)] = &[
    ("navigator.userAgent", "browser and version"),
    ("navigator.appVersion", "browser and version"),
    ("navigator.platform", "operating system"),
    ("navigator.oscpu", "operating system"),
    ("navigator.language", "languages"),
    ("navigator.languages", "languages"),
    ("navigator.hardwareConcurrency", "number of processor cores"),
    ("navigator.maxTouchPoints", "touch screen"),
    ("navigator.plugins", "plugins"),
    ("navigator.mimeTypes", "plugins"),
    ("navigator.webdriver", "whether it's run by a program"),
    ("navigator.doNotTrack", "Do Not Track setting"),
    ("navigator.pdfViewerEnabled", "PDF viewer"),
    ("navigator.vendor", "browser maker"),
    ("navigator.productSub", "browser maker"),
    ("screen.width", "screen size"),
    ("screen.height", "screen size"),
    ("screen.availWidth", "screen size"),
    ("screen.availHeight", "screen size"),
    ("screen.colorDepth", "colour depth"),
    ("screen.pixelDepth", "colour depth"),
    ("timezone", "time zone"),
    ("devicePixelRatio", "pixel density"),
];

/// Distinct details read before it looks like fingerprinting rather than
/// a site checking one thing (like your language).
const FINGERPRINT_MIN: usize = 6;

/// A line for the request panel when a page's scripts read enough details
/// of the browser to look like fingerprinting. `read` holds names from
/// [`FINGERPRINT`].
pub fn fingerprinting<'a>(read: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut what: Vec<&str> = Vec::new();
    for name in read {
        if let Some((_, label)) = FINGERPRINT.iter().find(|(n, _)| *n == name) {
            if !what.contains(label) {
                what.push(label);
            }
        }
    }
    (what.len() >= FINGERPRINT_MIN).then(|| {
        format!(
            "This page's scripts read your {}: together, details like these can recognise you \
             without cookies (fingerprinting). Brickle gives every copy of itself the same answers",
            what.join(", ")
        )
    })
}

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
        // Names ending in `_` (and long ones) also name the cookies that
        // start with them (`_ga_ABC123`, `mp_<token>_mixpanel`).
        let known = COOKIES
            .iter()
            .find(|(n, _)| c == n || ((n.len() > 4 || n.ends_with('_')) && c.starts_with(n)));
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

    // Code from another site runs with the page's own powers.
    if third_party && sent && e.dest == crate::net::Dest::Script {
        out.push(format!(
            "Code from another site ({}) running in this page: it can read and change anything on it \
             (Brickle stops it sending to other sites)",
            crate::cookies::site(&e.url)
        ));
    }
    let refused: Vec<&str> = e
        .cookies_refused
        .iter()
        .filter_map(|c| c.strip_suffix(" (third-party)"))
        .take(6)
        .collect();
    if !refused.is_empty() {
        out.push(format!(
            "Tried to set cookies ({}) that would recognise you on other sites; Brickle doesn't keep \
             cookies from other sites, so they were thrown away",
            refused
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if e.upgraded {
        out.push("Asked for without encryption: Brickle switched it to HTTPS, so others on your network can't read or change it".into());
    }
    if e.insecure {
        out.push(
            "Loaded without encryption (you allowed it): others on your network can read and change it"
                .into(),
        );
    }
    if e.cors_blocked {
        out.push("The other site didn't let this page read its reply, so Brickle kept the reply from the page's scripts".into());
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
    fn the_tables_are_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for (i, t) in TRACKERS.iter().enumerate() {
            assert!(seen.insert(t.at), "{} twice", t.at);
            assert_eq!(t.at, t.at.to_ascii_lowercase(), "{}", t.at);
            assert!(!t.what.ends_with('.') && !t.what.is_empty(), "{}", t.at);
            // A host-wide entry must come after any with a path on that host.
            if !t.at.contains('/') {
                for later in &TRACKERS[i + 1..] {
                    if let Some((host, _)) = later.at.split_once('/') {
                        assert!(
                            !(host == t.at || host.ends_with(&format!(".{}", t.at))),
                            "{} shadows {}",
                            t.at,
                            later.at
                        );
                    }
                }
            }
        }
        let mut names = std::collections::HashSet::new();
        for (n, _) in COOKIES {
            assert!(names.insert(*n), "cookie {n} twice");
        }
        for (n, _) in FINGERPRINT {
            assert!(
                n.starts_with("navigator.") || n.starts_with("screen.") || !n.contains('.'),
                "{n}"
            );
        }
    }

    #[test]
    fn more_of_what_requests_did() {
        let mut e = entry(
            "https://cdn.thirdparty.example/lib.js",
            Dest::Script,
            Outcome::Loaded { status: 200 },
        );
        e.cookies_refused = vec!["uid (third-party)".into(), "x (expired)".into()];
        e.upgraded = true;
        let all = explain(&e, "https://news.example/").join("\n");
        assert!(
            all.contains("Code from another site (thirdparty.example)"),
            "{all}"
        );
        assert!(all.contains("Tried to set cookies (`uid`)"), "{all}");
        assert!(!all.contains("`x`"), "only third-party refusals: {all}");
        assert!(all.contains("switched it to HTTPS"), "{all}");
        let own = entry(
            "https://news.example/app.js",
            Dest::Script,
            Outcome::Loaded { status: 200 },
        );
        assert!(
            explain(&own, "https://news.example/").is_empty(),
            "the site's own code"
        );
        // Prefix cookie names.
        let mut c = entry(
            "https://shop.example/",
            Dest::Document,
            Outcome::Loaded { status: 200 },
        );
        c.cookies_sent = vec!["mp_0123abcd_mixpanel".into(), "amp_6e403e".into()];
        let all = explain(&c, "https://shop.example/").join("\n");
        assert!(all.contains("Mixpanel") && all.contains("Amplitude"), "{all}");
        // A tracker that isn't a domain on its own.
        let t = tracker("https://www.google.com/recaptcha/api.js").unwrap();
        assert_eq!(t.kind, Kind::BotDetection);
        assert!(tracker("https://www.google.com/search?q=x").is_none());
    }

    #[test]
    fn fingerprinting_needs_many_details() {
        assert_eq!(
            fingerprinting(["navigator.language", "navigator.languages"]),
            None
        );
        assert_eq!(fingerprinting(["screen.width", "screen.height", "bogus"]), None);
        let line = fingerprinting([
            "navigator.userAgent",
            "navigator.platform",
            "navigator.hardwareConcurrency",
            "screen.width",
            "screen.height",
            "timezone",
            "navigator.plugins",
        ])
        .unwrap();
        assert!(line.contains("screen size, time zone, plugins"), "{line}");
        assert!(line.contains("fingerprinting"), "{line}");
        assert!(!line.contains("screen size, screen size"), "{line}");
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
