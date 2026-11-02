//! The Document Object Model: a tree of element and text nodes.

use std::collections::HashMap;

pub type AttrMap = HashMap<String, String>;

#[derive(Debug, Clone)]
pub struct Node {
    pub children: Vec<Node>,
    pub node_type: NodeType,
}

#[derive(Debug, Clone)]
pub enum NodeType {
    Text(String),
    Element(ElementData),
}

#[derive(Debug, Clone)]
pub struct ElementData {
    pub tag: String,
    pub attrs: AttrMap,
}

impl ElementData {
    pub fn id(&self) -> Option<&str> {
        self.attrs.get("id").map(|s| s.as_str())
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attrs
            .get("class")
            .map(|s| s.split_ascii_whitespace())
            .into_iter()
            .flatten()
    }

    pub fn has_class(&self, c: &str) -> bool {
        self.classes().any(|x| x == c)
    }
}

pub fn text(data: String) -> Node {
    Node { children: Vec::new(), node_type: NodeType::Text(data) }
}

pub fn elem(tag: String, attrs: AttrMap, children: Vec<Node>) -> Node {
    Node { children, node_type: NodeType::Element(ElementData { tag, attrs }) }
}

impl Node {
    pub fn element(&self) -> Option<&ElementData> {
        match &self.node_type {
            NodeType::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn tag(&self) -> Option<&str> {
        self.element().map(|e| e.tag.as_str())
    }

    /// Depth-first search for elements matching a predicate.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Node>, pred: &dyn Fn(&Node) -> bool) {
        if pred(self) {
            out.push(self);
        }
        for c in &self.children {
            c.walk(out, pred);
        }
    }

    /// Concatenated text content of this subtree.
    pub fn text_content(&self) -> String {
        match &self.node_type {
            NodeType::Text(t) => t.clone(),
            NodeType::Element(_) => self.children.iter().map(|c| c.text_content()).collect(),
        }
    }

    /// Pretty-print the tree (for `brickle dom`).
    pub fn dump(&self, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        match &self.node_type {
            NodeType::Text(t) => {
                let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                if !t.is_empty() {
                    out.push_str(&format!("{pad}\"{t}\"\n"));
                }
            }
            NodeType::Element(e) => {
                let mut attrs: Vec<_> = e.attrs.iter().collect();
                attrs.sort();
                let a: String = attrs.iter().map(|(k, v)| format!(" {k}=\"{v}\"")).collect();
                out.push_str(&format!("{pad}<{}{a}>\n", e.tag));
                for c in &self.children {
                    c.dump(depth + 1, out);
                }
            }
        }
    }
}
