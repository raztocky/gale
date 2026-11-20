//! Form validation, as browsers do it before a form is sent: `required`,
//! `type=email`/`url`, `pattern`, `minlength`/`maxlength`, and
//! `min`/`max`/`step` for numbers, plus scripts' `setCustomValidity()`.
//! The messages are Firefox's.

use crate::forms::{self, Control};
use crate::js::Dom;
use std::collections::HashMap;

/// Longest `pattern`, and longest value matched against one: a page's
/// pattern can't make matching take forever.
const MAX_PATTERN: usize = 1000;
const MAX_MATCHED: usize = 10_000;

/// What's wrong with a field (the `ValidityState` flags).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Validity {
    pub value_missing: bool,
    pub type_mismatch: bool,
    pub pattern_mismatch: bool,
    pub too_long: bool,
    pub too_short: bool,
    pub range_underflow: bool,
    pub range_overflow: bool,
    pub step_mismatch: bool,
    pub bad_input: bool,
    pub custom_error: bool,
    /// What a browser would say about it ("" when valid).
    pub message: String,
}

impl Validity {
    pub fn valid(&self) -> bool {
        self.message.is_empty()
    }
}

/// An input's `type`, lowercased.
fn input_type(dom: &Dom, id: u32) -> String {
    dom.element(id)
        .and_then(|e| e.attrs.get("type"))
        .map(|t| t.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "text".into())
}

fn attr<'a>(dom: &'a Dom, id: u32, name: &str) -> Option<&'a str> {
    dom.element(id)
        .and_then(|e| e.attrs.get(name))
        .map(String::as_str)
}

/// Whether the field takes part in validation at all (`willValidate`).
pub fn will_validate(dom: &Dom, id: u32) -> bool {
    let Some(kind) = forms::kind(dom, id) else {
        return false;
    };
    if forms::disabled(dom, id) || in_datalist(dom, id) {
        return false;
    }
    match kind {
        Control::Text { .. } | Control::TextArea => attr(dom, id, "readonly").is_none(),
        Control::Checkbox | Control::Radio | Control::Select | Control::File | Control::Color => true,
        // Submit buttons can carry a custom error; nothing else applies.
        Control::Submit => true,
        Control::Reset | Control::Button | Control::Hidden => false,
    }
}

fn in_datalist(dom: &Dom, id: u32) -> bool {
    let mut cur = dom.parent(id);
    while let Some(p) = cur {
        if dom.element(p).is_some_and(|e| e.tag == "datalist") {
            return true;
        }
        cur = dom.parent(p);
    }
    false
}

fn checked(dom: &Dom, id: u32) -> bool {
    attr(dom, id, "checked").is_some()
}

/// A radio group has a checked button (same name, same form).
fn group_checked(dom: &Dom, id: u32) -> bool {
    let Some(name) = attr(dom, id, "name").filter(|n| !n.is_empty()) else {
        return checked(dom, id);
    };
    let form = forms::form_of(dom, id);
    let mut all = Vec::new();
    dom.descendants(0, &mut all);
    all.into_iter().any(|o| {
        forms::kind(dom, o) == Some(Control::Radio)
            && attr(dom, o, "name") == Some(name)
            && forms::form_of(dom, o) == form
            && checked(dom, o)
    })
}

fn group_required(dom: &Dom, id: u32) -> bool {
    let Some(name) = attr(dom, id, "name").filter(|n| !n.is_empty()) else {
        return attr(dom, id, "required").is_some();
    };
    let form = forms::form_of(dom, id);
    let mut all = Vec::new();
    dom.descendants(0, &mut all);
    all.into_iter().any(|o| {
        forms::kind(dom, o) == Some(Control::Radio)
            && attr(dom, o, "name") == Some(name)
            && forms::form_of(dom, o) == form
            && attr(dom, o, "required").is_some()
    })
}

/// A plausible email address: something@something, no spaces.
fn email_ok(v: &str) -> bool {
    let Some((local, domain)) = v.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(c));
    let domain_ok = !domain.is_empty()
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    local_ok && domain_ok
}

/// An absolute URL: a scheme, a colon, and something after it.
fn url_ok(v: &str) -> bool {
    url::Url::parse(v).is_ok()
}

fn number(s: &str) -> Option<f64> {
    let s = s.trim();
    // HTML numbers: no "inf", "nan", hex or leading "+".
    if s.is_empty()
        || s.starts_with('+')
        || s.chars().any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
    {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn show(v: f64) -> String {
    let r = (v * 1e9).round() / 1e9;
    if r == r.trunc() && r.abs() < 1e15 {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

fn pattern_ok(pattern: &str, value: &str) -> bool {
    if pattern.len() > MAX_PATTERN || value.len() > MAX_MATCHED {
        return true; // too big to check safely: let it through
    }
    match regress::Regex::with_flags(&format!("^(?:{pattern})$"), "u") {
        Ok(re) => re.find(value).is_some(),
        Err(_) => true, // a pattern that isn't valid is ignored, as in browsers
    }
}

/// How `id` stands. `custom` holds scripts' `setCustomValidity()` messages.
pub fn validity(dom: &Dom, id: u32, custom: &HashMap<u32, String>) -> Validity {
    let mut v = Validity::default();
    if !will_validate(dom, id) {
        return v;
    }
    let kind = forms::kind(dom, id);
    let value = forms::value(dom, id);
    let required = attr(dom, id, "required").is_some();
    let ty = input_type(dom, id);
    let mut msg = String::new();
    let mut say = |m: String| {
        if msg.is_empty() {
            msg = m;
        }
    };
    if let Some(c) = custom.get(&id).filter(|c| !c.is_empty()) {
        v.custom_error = true;
        say(c.clone());
    }
    match kind {
        Some(Control::Checkbox) => {
            if required && !checked(dom, id) {
                v.value_missing = true;
                say("Please check this box if you want to proceed.".into());
            }
        }
        Some(Control::Radio) => {
            if group_required(dom, id) && !group_checked(dom, id) {
                v.value_missing = true;
                say("Please select one of these options.".into());
            }
        }
        Some(Control::Select) => {
            if required && value.is_empty() {
                v.value_missing = true;
                say("Please select an item in the list.".into());
            }
        }
        Some(Control::File) => {
            if required && !dom.files.contains_key(&id) {
                v.value_missing = true;
                say("Please select a file.".into());
            }
        }
        Some(Control::Text { .. } | Control::TextArea) => {
            if required && value.is_empty() {
                v.value_missing = true;
                say("Please fill out this field.".into());
            }
            if !value.is_empty() {
                let multiple = attr(dom, id, "multiple").is_some();
                if ty == "email" {
                    let ok = if multiple {
                        value.split(',').all(|p| email_ok(p.trim()))
                    } else {
                        email_ok(&value)
                    };
                    if !ok {
                        v.type_mismatch = true;
                        say("Please enter an email address.".into());
                    }
                } else if ty == "url" && !url_ok(&value) {
                    v.type_mismatch = true;
                    say("Please enter a URL.".into());
                }
                if let Some(p) = attr(dom, id, "pattern").filter(|_| kind != Some(Control::TextArea)) {
                    let all_match = if ty == "email" && multiple {
                        value.split(',').all(|part| pattern_ok(p, part.trim()))
                    } else {
                        pattern_ok(p, &value)
                    };
                    if !all_match {
                        v.pattern_mismatch = true;
                        let title = attr(dom, id, "title").map(str::trim).filter(|t| !t.is_empty());
                        say(match title {
                            Some(t) => format!("Please match the requested format: {t}."),
                            None => "Please match the requested format.".into(),
                        });
                    }
                }
                let len = value.encode_utf16().count();
                let limit = |a: &str| attr(dom, id, a).and_then(|n| n.trim().parse::<usize>().ok());
                if let Some(max) = limit("maxlength").filter(|&m| len > m) {
                    v.too_long = true;
                    say(format!(
                        "Please shorten this text to {max} characters or less (you are currently using {len} characters)."
                    ));
                }
                if let Some(min) = limit("minlength").filter(|&m| len < m) {
                    v.too_short = true;
                    say(format!(
                        "Please use at least {min} characters (you are currently using {len} characters)."
                    ));
                }
                if let Some(kind) = dom.element(id).and_then(crate::pickers::kind) {
                    match crate::pickers::parse(kind, &value) {
                        None => {
                            v.bad_input = true;
                            say(crate::pickers::bad_input_message(kind).into());
                        }
                        Some(k) => {
                            let (lo, hi) = dom
                                .element(id)
                                .map_or((None, None), |e| crate::pickers::bounds(e, kind));
                            if let Some(lo) = lo.filter(|&lo| k < lo) {
                                v.range_underflow = true;
                                say(format!(
                                    "Please select a value that is no earlier than {}.",
                                    crate::pickers::format(kind, lo)
                                ));
                            }
                            if let Some(hi) = hi.filter(|&hi| k > hi) {
                                v.range_overflow = true;
                                say(format!(
                                    "Please select a value that is no later than {}.",
                                    crate::pickers::format(kind, hi)
                                ));
                            }
                        }
                    }
                }
                if matches!(ty.as_str(), "number" | "range") {
                    match number(&value) {
                        None => {
                            v.bad_input = true;
                            say("Please enter a number.".into());
                        }
                        Some(n) => {
                            let min = attr(dom, id, "min").and_then(number);
                            let max = attr(dom, id, "max").and_then(number);
                            if let Some(lo) = min.filter(|&lo| n < lo) {
                                v.range_underflow = true;
                                say(format!(
                                    "Please select a value that is no less than {}.",
                                    show(lo)
                                ));
                            }
                            if let Some(hi) = max.filter(|&hi| n > hi) {
                                v.range_overflow = true;
                                say(format!(
                                    "Please select a value that is no more than {}.",
                                    show(hi)
                                ));
                            }
                            if let Some(step) = step_of(dom, id) {
                                let base = min.unwrap_or(0.0);
                                let k = (n - base) / step;
                                if (k - k.round()).abs() > 1e-7 {
                                    v.step_mismatch = true;
                                    let below = base + k.floor() * step;
                                    say(format!(
                                        "Please select a valid value. The two nearest valid values are {} and {}.",
                                        show(below),
                                        show(below + step)
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    v.message = msg;
    v
}

/// `:valid`/`:invalid` for element `id`: a field that's checked, or a form
/// (valid when all its fields are). None for anything else.
pub fn state(dom: &Dom, id: u32) -> Option<bool> {
    let e = dom.element(id)?;
    if e.tag == "form" {
        let fields = forms::controls(dom, id);
        return Some(
            fields
                .into_iter()
                .all(|c| validity(dom, c, &dom.custom_validity).valid()),
        );
    }
    will_validate(dom, id).then(|| validity(dom, id, &dom.custom_validity).valid())
}

/// Most fields marked `:valid`/`:invalid` for styling; past it a page's
/// fields are left unmarked (checking radio groups looks at the whole page).
const MAX_MARKED: usize = 2000;

/// `:valid`/`:invalid` for every form and checked field, by node.
pub fn marks(dom: &Dom) -> HashMap<u32, bool> {
    let mut all = Vec::new();
    dom.descendants(0, &mut all);
    let candidates: Vec<u32> = all
        .into_iter()
        .filter(|&n| dom.element(n).is_some_and(|e| e.tag == "form") || forms::kind(dom, n).is_some())
        .collect();
    if candidates.len() > MAX_MARKED {
        return HashMap::new();
    }
    candidates
        .into_iter()
        .filter_map(|n| state(dom, n).map(|v| (n, v)))
        .collect()
}

/// A number field's step (None for `step="any"`); 1 by default.
pub fn step_of(dom: &Dom, id: u32) -> Option<f64> {
    match attr(dom, id, "step").map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("any") => None,
        Some(s) => Some(number(s).filter(|&v| v > 0.0).unwrap_or(1.0)),
        None => Some(1.0),
    }
}

/// The invalid fields of `form` in document order, with their messages;
/// none if the form (or the button sending it) says not to validate.
pub fn invalid_fields(
    dom: &Dom,
    form: u32,
    submitter: Option<u32>,
    custom: &HashMap<u32, String>,
) -> Vec<(u32, String)> {
    if attr(dom, form, "novalidate").is_some()
        || submitter.is_some_and(|b| attr(dom, b, "formnovalidate").is_some())
    {
        return Vec::new();
    }
    forms::controls(dom, form)
        .into_iter()
        .filter_map(|c| {
            let v = validity(dom, c, custom);
            (!v.valid()).then_some((c, v.message))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dom(html: &str) -> Dom {
        Dom::from_tree(&crate::html::parse(html))
    }

    fn by_id(d: &Dom, id: &str) -> u32 {
        forms::by_id(d, id).expect(id)
    }

    fn message(html: &str, id: &str) -> String {
        let d = dom(html);
        validity(&d, by_id(&d, id), &HashMap::new()).message
    }

    #[test]
    fn required_fields() {
        assert_eq!(
            message("<input id=a required>", "a"),
            "Please fill out this field."
        );
        assert_eq!(message("<input id=a required value=x>", "a"), "");
        assert_eq!(
            message("<input id=a required disabled>", "a"),
            "",
            "disabled fields aren't checked"
        );
        assert_eq!(message("<input id=a required readonly>", "a"), "");
        assert_eq!(
            message("<input id=a type=checkbox required>", "a"),
            "Please check this box if you want to proceed."
        );
        let radios = "<form><input id=a type=radio name=r required><input id=b type=radio name=r></form>";
        assert_eq!(
            message(radios, "b"),
            "Please select one of these options.",
            "the whole group"
        );
        let radios =
            "<form><input id=a type=radio name=r required><input id=b type=radio name=r checked></form>";
        assert_eq!(message(radios, "a"), "");
        assert_eq!(
            message(
                "<select id=a required><option value=''>Pick</option><option>x</option></select>",
                "a"
            ),
            "Please select an item in the list."
        );
    }

    #[test]
    fn types_patterns_and_lengths() {
        assert_eq!(
            message("<input id=a type=email value=nope>", "a"),
            "Please enter an email address."
        );
        assert_eq!(message("<input id=a type=email value=a@b.example>", "a"), "");
        assert_eq!(
            message("<input id=a type=url value='not a url'>", "a"),
            "Please enter a URL."
        );
        assert_eq!(
            message("<input id=a type=url value='https://x.example/'>", "a"),
            ""
        );
        assert_eq!(
            message(
                "<input id=a pattern='[0-9]{3}' title='Three digits' value=12a>",
                "a"
            ),
            "Please match the requested format: Three digits."
        );
        assert_eq!(message("<input id=a pattern='[0-9]{3}' value=123>", "a"), "");
        assert_eq!(
            message("<input id=a pattern='(' value=x>", "a"),
            "",
            "a broken pattern is ignored"
        );
        assert_eq!(
            message("<input id=a minlength=4 value=abc>", "a"),
            "Please use at least 4 characters (you are currently using 3 characters)."
        );
        assert_eq!(
            message("<input id=a minlength=4 value=''>", "a"),
            "",
            "empty isn't too short"
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            message("<input id=a type=number min=1 max=5 value=0>", "a"),
            "Please select a value that is no less than 1."
        );
        assert_eq!(
            message("<input id=a type=number max=5 value=9>", "a"),
            "Please select a value that is no more than 5."
        );
        assert_eq!(
            message("<input id=a type=number step=0.5 value=1.2>", "a"),
            "Please select a valid value. The two nearest valid values are 1 and 1.5."
        );
        assert_eq!(message("<input id=a type=number step=any value=1.2>", "a"), "");
        assert_eq!(
            message("<input id=a type=number value=abc>", "a"),
            "Please enter a number."
        );
        assert_eq!(
            message("<input id=a type=date value=2026-02-30>", "a"),
            "Please enter a valid date."
        );
        assert_eq!(
            message("<input id=a type=date min=2026-01-01 value=2025-12-31>", "a"),
            "Please select a value that is no earlier than 2026-01-01."
        );
        assert_eq!(
            message("<input id=a type=date required value=2026-01-01>", "a"),
            ""
        );
    }

    #[test]
    fn custom_messages_and_novalidate() {
        let d = dom("<form id=f><input id=a></form>");
        let a = by_id(&d, "a");
        let custom = HashMap::from([(a, "Taken already".to_string())]);
        assert_eq!(validity(&d, a, &custom).message, "Taken already");
        assert!(validity(&d, a, &custom).custom_error);
        let f = by_id(&d, "f");
        assert_eq!(
            invalid_fields(&d, f, None, &custom),
            [(a, "Taken already".to_string())]
        );
        let d = dom("<form id=f novalidate><input required></form>");
        assert!(invalid_fields(&d, by_id(&d, "f"), None, &HashMap::new()).is_empty());
        let d = dom("<form id=f><input required><button id=b formnovalidate>Save draft</button></form>");
        assert!(invalid_fields(&d, by_id(&d, "f"), Some(by_id(&d, "b")), &HashMap::new()).is_empty());
    }
}
