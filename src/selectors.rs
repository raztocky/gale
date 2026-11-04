//! Matching CSS selectors against a document. The style system and the
//! script DOM keep their trees differently; both describe theirs through
//! [`Tree`], so they share one matcher.

use crate::css::{AttrOp, AttrSelector, Combinator, Pseudo, PseudoElement, Selector, SimpleSelector};
use crate::dom::{ElementData, Node, NodeType};
use std::cell::Cell;

/// What matching needs to know about a document whose nodes have `u32` ids.
pub trait Tree {
    /// The element `id` is, if it is one.
    fn element(&self, id: u32) -> Option<&ElementData>;
    /// `id`'s parent node, whatever it is.
    fn parent_node(&self, id: u32) -> Option<u32>;
    fn children(&self, id: u32) -> &[u32];
    /// `id` is a text node with something in it.
    fn has_text(&self, id: u32) -> bool;
    /// Where `id` is in its parent's children.
    fn index(&self, id: u32) -> usize {
        self.parent_node(id)
            .and_then(|p| self.children(p).iter().position(|&c| c == id))
            .unwrap_or(0)
    }
}

/// Most matching steps (ancestors, siblings and descendants looked at)
/// between [`reset_budget`] calls. Past it, selectors that need more
/// stop matching, so a crafted stylesheet (`:has()` everywhere, long
/// sibling chains) can't stall the renderer; ordinary pages use a small
/// fraction.
const BUDGET: u64 = 50_000_000;

thread_local! {
    static STEPS: Cell<u64> = const { Cell::new(0) };
}

/// Start a new budget (once per styling pass or script query).
pub fn reset_budget() {
    STEPS.with(|s| s.set(0));
}

/// Count a step; false once the budget is spent.
fn step() -> bool {
    STEPS.with(|s| {
        let n = s.get() + 1;
        s.set(n);
        n <= BUDGET
    })
}

/// The parent, if it's an element.
fn parent<T: Tree + ?Sized>(t: &T, id: u32) -> Option<u32> {
    t.parent_node(id).filter(|&p| t.element(p).is_some())
}

/// The element siblings before `id`, nearest first.
fn previous<T: Tree + ?Sized>(t: &T, id: u32) -> impl Iterator<Item = u32> + '_ {
    let sibs = t.parent_node(id).map_or(&[][..], |p| t.children(p));
    let at = t.index(id).min(sibs.len());
    sibs[..at]
        .iter()
        .rev()
        .copied()
        .filter(move |&s| t.element(s).is_some())
}

/// Does element `id` match `sel`? Selectors for `::before`/`::after` never
/// match an element itself.
pub fn matches<T: Tree + ?Sized>(t: &T, id: u32, sel: &Selector) -> bool {
    sel.parts[0].0.pseudo_element.is_none() && t.element(id).is_some() && go(t, &sel.parts, id, None)
}

/// Does `sel` style element `id`'s `pe` box?
pub fn matches_pseudo<T: Tree + ?Sized>(t: &T, id: u32, sel: &Selector, pe: PseudoElement) -> bool {
    sel.parts[0].0.pseudo_element == Some(pe) && t.element(id).is_some() && go(t, &sel.parts, id, None)
}

/// Does `id` match `parts` (right to left)? With a `scope`, `parts` is a
/// relative selector and its leftmost part must relate to the scope by its
/// combinator.
fn go<T: Tree + ?Sized>(t: &T, parts: &[(SimpleSelector, Combinator)], id: u32, scope: Option<u32>) -> bool {
    let (simple, comb) = &parts[0];
    if !compound(t, id, simple) {
        return false;
    }
    let rest = &parts[1..];
    if rest.is_empty() {
        return scope.is_none_or(|sc| related(t, id, *comb, sc));
    }
    match comb {
        Combinator::Child => parent(t, id).is_some_and(|p| step() && go(t, rest, p, scope)),
        Combinator::Next => previous(t, id)
            .next()
            .is_some_and(|s| step() && go(t, rest, s, scope)),
        Combinator::Subsequent => previous(t, id).any(|s| step() && go(t, rest, s, scope)),
        Combinator::Descendant | Combinator::None => {
            // With only descendant combinators left, the nearest ancestor
            // that matches is always the best choice, so no backtracking:
            // crafted selectors like `a a a a b` stay linear.
            let greedy = rest
                .iter()
                .all(|(_, c)| matches!(c, Combinator::Descendant | Combinator::None));
            let mut p = parent(t, id);
            while let Some(a) = p {
                if !step() {
                    return false;
                }
                if greedy {
                    if compound(t, a, &rest[0].0) {
                        return go(t, rest, a, scope);
                    }
                } else if go(t, rest, a, scope) {
                    return true;
                }
                p = parent(t, a);
            }
            false
        }
    }
}

/// Is `id` related to `scope` the way a relative selector's leftmost
/// combinator says?
fn related<T: Tree + ?Sized>(t: &T, id: u32, comb: Combinator, scope: u32) -> bool {
    match comb {
        Combinator::Child => parent(t, id) == Some(scope),
        Combinator::Next => previous(t, id).next() == Some(scope),
        Combinator::Subsequent => previous(t, id).any(|s| s == scope),
        Combinator::Descendant | Combinator::None => {
            let mut p = t.parent_node(id);
            while let Some(a) = p {
                if a == scope {
                    return true;
                }
                if !step() {
                    return false;
                }
                p = t.parent_node(a);
            }
            false
        }
    }
}

fn compound<T: Tree + ?Sized>(t: &T, id: u32, s: &SimpleSelector) -> bool {
    let Some(e) = t.element(id) else {
        return false;
    };
    if s.tag.as_ref().is_some_and(|tag| *tag != e.tag) {
        return false;
    }
    if s.id.as_ref().is_some_and(|i| Some(i.as_str()) != e.id()) {
        return false;
    }
    if s.root && !(parent(t, id).is_none() && e.tag == "html") {
        return false;
    }
    if !s.classes.iter().all(|c| e.has_class(c)) || !s.attrs.iter().all(|a| attr_matches(e, a)) {
        return false;
    }
    s.pseudo.iter().all(|p| pseudo(t, id, e, p))
}

fn attr_matches(e: &ElementData, a: &AttrSelector) -> bool {
    let Some(v) = e.attrs.get(&a.name) else {
        return false;
    };
    let (v, want) = if a.ignore_case {
        (v.to_ascii_lowercase(), a.value.to_ascii_lowercase())
    } else {
        (v.clone(), a.value.clone())
    };
    match a.op {
        AttrOp::Exists => true,
        AttrOp::Equals => v == want,
        AttrOp::Word => !want.is_empty() && v.split_ascii_whitespace().any(|w| w == want),
        AttrOp::Lang => v == want || v.strip_prefix(&want).is_some_and(|r| r.starts_with('-')),
        // An empty value matches nothing for these.
        AttrOp::Prefix => !want.is_empty() && v.starts_with(&want),
        AttrOp::Suffix => !want.is_empty() && v.ends_with(&want),
        AttrOp::Contains => !want.is_empty() && v.contains(&want),
    }
}

fn pseudo<T: Tree + ?Sized>(t: &T, id: u32, e: &ElementData, p: &Pseudo) -> bool {
    // The element siblings that count (of the same type, if asked), and
    // where `id` is among them.
    let position = |of_type: bool| -> (usize, usize) {
        let sibs = t.parent_node(id).map_or(&[][..], |p| t.children(p));
        let counts = |s: &u32| t.element(*s).is_some_and(|x| !of_type || x.tag == e.tag);
        let at = t.index(id).min(sibs.len());
        let before = sibs[..at].iter().filter(|s| counts(s)).count();
        let after = sibs[at..].iter().skip(1).filter(|s| counts(s)).count();
        (before, after)
    };
    match p {
        Pseudo::Nth {
            a,
            b,
            of_type,
            from_end,
        } => {
            let (before, after) = position(*of_type);
            let pos = 1 + if *from_end { after } else { before } as i64;
            let (a, b) = (*a as i64, *b as i64);
            if a == 0 {
                pos == b
            } else {
                (pos - b) % a == 0 && (pos - b) / a >= 0
            }
        }
        Pseudo::Only { of_type } => position(*of_type) == (0, 0),
        Pseudo::Empty => t
            .children(id)
            .iter()
            .all(|&c| t.element(c).is_none() && !t.has_text(c)),
        Pseudo::Not(list) => !list.iter().any(|s| go(t, &s.parts, id, None)),
        Pseudo::Is { list, .. } => list.iter().any(|s| go(t, &s.parts, id, None)),
        Pseudo::Has(list) => list.iter().any(|rel| {
            let lead = rel.parts.last().map_or(Combinator::Descendant, |p| p.1);
            // Where a match can be: inside `id`, or (for `+`/`~`) in or
            // after its following siblings.
            let mut roots: Vec<u32> = Vec::new();
            if matches!(lead, Combinator::Next | Combinator::Subsequent) {
                let sibs = t.parent_node(id).map_or(&[][..], |p| t.children(p));
                roots.extend(sibs.iter().skip(t.index(id) + 1));
            } else {
                roots.extend(t.children(id));
            }
            let mut stack: Vec<u32> = roots.into_iter().rev().collect();
            while let Some(n) = stack.pop() {
                if !step() {
                    return false;
                }
                if go(t, &rel.parts, n, Some(id)) {
                    return true;
                }
                stack.extend(t.children(n).iter().rev());
            }
            false
        }),
        Pseudo::Never => false,
    }
}

/// A [`Node`] tree with ids (in tree order), parents and positions, for
/// matching against while styling.
pub struct Flat<'a> {
    nodes: Vec<FlatNode<'a>>,
}

struct FlatNode<'a> {
    node: &'a Node,
    parent: Option<u32>,
    index: usize,
    children: Vec<u32>,
}

impl<'a> Flat<'a> {
    /// Ids are given in tree order (a node, then its children), starting at
    /// 0 for `root`.
    pub fn new(root: &'a Node) -> Flat<'a> {
        let mut f = Flat { nodes: Vec::new() };
        // Iterative, so a deep document can't overflow the stack.
        let mut stack: Vec<(&Node, Option<u32>, usize)> = vec![(root, None, 0)];
        while let Some((node, parent, index)) = stack.pop() {
            let id = f.nodes.len() as u32;
            f.nodes.push(FlatNode {
                node,
                parent,
                index,
                children: Vec::new(),
            });
            if let Some(p) = parent {
                f.nodes[p as usize].children.push(id);
            }
            for (i, c) in node.children.iter().enumerate().rev() {
                stack.push((c, Some(id), i));
            }
        }
        f
    }
}

impl Tree for Flat<'_> {
    fn element(&self, id: u32) -> Option<&ElementData> {
        match &self.nodes.get(id as usize)?.node.node_type {
            NodeType::Element(e) => Some(e),
            _ => None,
        }
    }
    fn parent_node(&self, id: u32) -> Option<u32> {
        self.nodes.get(id as usize)?.parent
    }
    fn children(&self, id: u32) -> &[u32] {
        self.nodes.get(id as usize).map_or(&[], |n| &n.children)
    }
    fn has_text(&self, id: u32) -> bool {
        self.nodes
            .get(id as usize)
            .is_some_and(|n| matches!(&n.node.node_type, NodeType::Text(t) if !t.is_empty()))
    }
    fn index(&self, id: u32) -> usize {
        self.nodes.get(id as usize).map_or(0, |n| n.index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css;

    /// The ids of elements in `html` matching `sel`, as "tag#id" (or tag).
    fn select(html: &str, sel: &str) -> Vec<String> {
        let dom = crate::html::parse(html);
        let flat = Flat::new(&dom);
        let sels = css::parse_selector_list(sel);
        assert!(!sels.is_empty(), "didn't parse: {sel}");
        reset_budget();
        (0..flat.nodes.len() as u32)
            .filter(|&id| sels.iter().any(|s| matches(&flat, id, s)))
            .filter_map(|id| flat.element(id))
            .map(|e| e.id().map_or(e.tag.clone(), |i| format!("{}#{i}", e.tag)))
            .collect()
    }

    const LIST: &str = "<ul><li id=a>1</li><li id=b class=x>2</li><li id=c>3</li>\
                        <li id=d class=x>4</li><li id=e>5</li></ul>";

    #[test]
    fn structural_pseudo_classes() {
        assert_eq!(select(LIST, "li:first-child"), ["li#a"]);
        assert_eq!(select(LIST, "li:last-child"), ["li#e"]);
        assert_eq!(select(LIST, "li:nth-child(odd)"), ["li#a", "li#c", "li#e"]);
        assert_eq!(select(LIST, "li:nth-child(2n)"), ["li#b", "li#d"]);
        assert_eq!(select(LIST, "li:nth-child(-n+2)"), ["li#a", "li#b"]);
        assert_eq!(select(LIST, "li:nth-last-child(2)"), ["li#d"]);
        assert_eq!(select(LIST, "li:nth-child( 3 )"), ["li#c"]);
        let mixed = "<div><p id=p1></p><span id=s1>x</span><p id=p2></p><p id=p3>y</p></div>";
        assert_eq!(select(mixed, "p:first-of-type"), ["p#p1"]);
        assert_eq!(select(mixed, "p:nth-of-type(2)"), ["p#p2"]);
        assert_eq!(select(mixed, "span:only-of-type"), ["span#s1"]);
        assert_eq!(select(mixed, "p:empty"), ["p#p1", "p#p2"]);
        assert!(select(mixed, "p:only-child").is_empty());
    }

    #[test]
    fn logical_pseudo_classes() {
        assert_eq!(select(LIST, "li:not(.x)"), ["li#a", "li#c", "li#e"]);
        assert_eq!(select(LIST, "li:not(.x, :first-child)"), ["li#c", "li#e"]);
        assert_eq!(select(LIST, ":is(#a, #e)"), ["li#a", "li#e"]);
        assert_eq!(select(LIST, "ul :where(.x)"), ["li#b", "li#d"]);
        // A state the page isn't in: :not() of it matches.
        assert_eq!(select(LIST, "li:hover").len(), 0);
        assert_eq!(select(LIST, "li:not(:hover)").len(), 5);
    }

    #[test]
    fn has_and_sibling_combinators() {
        let doc = "<div id=d1><img id=i></div><div id=d2><p><img></p></div><div id=d3></div>";
        assert_eq!(select(doc, "div:has(img)"), ["div#d1", "div#d2"]);
        assert_eq!(select(doc, "div:has(> img)"), ["div#d1"]);
        assert_eq!(select(doc, "div:has(+ div:empty)"), ["div#d2"]);
        assert_eq!(select(doc, "div:not(:has(img))"), ["div#d3"]);
        assert_eq!(select(LIST, ".x + li"), ["li#c", "li#e"]);
        assert_eq!(select(LIST, "#c ~ li"), ["li#d", "li#e"]);
        assert_eq!(select(LIST, "li+li+li+li"), ["li#d", "li#e"]);
    }

    #[test]
    fn attribute_values() {
        let doc = "<a id=a href='https://x.example/a.pdf' lang=en-US class='btn big'></a>\
                   <a id=b href='/b' data-x=''></a><input id=c type=TEXT>";
        assert_eq!(select(doc, "[href^='https']"), ["a#a"]);
        assert_eq!(select(doc, "a[href$=\".pdf\"]"), ["a#a"]);
        assert_eq!(select(doc, "[href*=b]"), ["a#b"]);
        assert_eq!(select(doc, "[class~=big]"), ["a#a"]);
        assert_eq!(select(doc, "[lang|=en]"), ["a#a"]);
        assert_eq!(select(doc, "[data-x]"), ["a#b"]);
        assert_eq!(select(doc, "input[type=text i]"), ["input#c"]);
        assert!(select(doc, "input[type=text]").is_empty());
        assert!(
            select(doc, "[href^='']").is_empty(),
            "an empty prefix matches nothing"
        );
    }

    #[test]
    fn specificity_of_the_new_selectors() {
        let spec = |s: &str| css::parse_selector(s).unwrap().specificity();
        assert_eq!(spec("li:first-child"), (0, 1, 1));
        assert_eq!(spec(":is(#a, .b)"), (1, 0, 0), "the most specific argument");
        assert_eq!(spec(":where(#a, .b) p"), (0, 0, 1), ":where adds nothing");
        assert_eq!(spec("a:not(.x)"), (0, 1, 1));
        assert_eq!(spec("[type=text]"), (0, 1, 0));
    }

    #[test]
    fn what_still_does_not_parse() {
        for bad in [
            "a::first-line",
            "a:unknown-thing",
            "li:nth-child(2n of .x)",
            ":not()",
            "a > > b",
            "a +",
            "[x=\"unclosed]",
            ":is(",
        ] {
            assert_eq!(css::parse_selector(bad), None, "{bad}");
        }
        // One bad selector doesn't sink the rule's others.
        assert_eq!(css::parse_selector_list("a::marker, b, c:hover").len(), 2);
    }

    #[test]
    fn crafted_selectors_stay_bounded() {
        // :has() on every element of a deep tree, and long sibling chains:
        // the budget stops them instead of running for minutes.
        let deep = format!("{}{}", "<div>".repeat(2000), "</div>".repeat(2000));
        let start = std::time::Instant::now();
        let _ = select(&deep, "div:has(div div div div span)");
        let wide = format!("<ul>{}</ul>", "<li></li>".repeat(3000));
        let _ = select(&wide, "li ~ li ~ li ~ li ~ li:nth-child(odd)");
        assert!(start.elapsed().as_secs() < 30, "{:?}", start.elapsed());
        // Nesting is limited too.
        let nested = format!("{}a{}", ":not(".repeat(50), ")".repeat(50));
        assert_eq!(css::parse_selector(&nested), None);
    }
}
