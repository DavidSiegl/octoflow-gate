//! The XML reports the gate reads, JUnit and Cobertura, as a tree of elements, with the parts of Python's ElementTree
//! that the gate needs of one: a name, attributes, the text before the first child, and children.

use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// One element of a document, its descendants with it.
#[derive(Debug, Default)]
pub struct Element {
    pub name: String,
    /// The attributes in document order, their values unescaped and normalized as XML 1.0 says (a line end or a tab
    /// becomes a space), as ElementTree gives them.
    attributes: Vec<(String, String)>,
    /// The character data before the first child element, entities resolved, line ends made `\n`; ElementTree's
    /// `text`, but "" where it has None.
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    /// The value of the attribute `name`, None when the element has none.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The first child named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// The children named `name`, in order.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// This element and every element below it named `name`, in document order, as ElementTree's `iter(name)`.
    pub fn descendants<'a>(&'a self, name: &'a str) -> Vec<&'a Element> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(e) = stack.pop() {
            if e.name == name {
                out.push(e);
            }
            stack.extend(e.children.iter().rev());
        }
        out
    }
}

/// The root element of the XML document in the file at `path`; Err, saying why, when the file cannot be read, is not
/// UTF-8 or is not well-formed XML, or uses an entity XML does not predefine.
pub fn parse_file(path: &Path) -> Result<Element, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let source = String::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))?;
    parse(&source)
}

/// The root element of the XML document `source`, as [`parse_file`] says.
pub fn parse(source: &str) -> Result<Element, String> {
    let mut reader = Reader::from_str(source);
    reader.config_mut().check_end_names = true;
    // The open elements, innermost last; the document's root is the first.
    let mut open: Vec<Element> = Vec::new();
    let mut root = None;
    loop {
        let event = reader.read_event().map_err(|e| format!("at byte {}: {e}", reader.error_position()))?;
        match event {
            Event::Start(start) => open.push(element(&start)?),
            Event::Empty(start) => close(element(&start)?, &mut open, &mut root)?,
            Event::End(_) => {
                let done = open.pop().ok_or("an end tag closes no element")?;
                close(done, &mut open, &mut root)?;
            }
            Event::Text(t) => add_text(&mut open, &t.xml_content(XmlVersion::Implicit1_0)),
            Event::CData(t) => add_text(&mut open, &t.xml_content(XmlVersion::Implicit1_0)),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref().map_err(|e| e.to_string())? {
                    Some(c) => c.to_string(),
                    None => predefined(&r.xml_content(XmlVersion::Implicit1_0))?.to_string(),
                };
                add_text(&mut open, &resolved);
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    if !open.is_empty() {
        return Err("the document ends inside an element".into());
    }
    root.ok_or_else(|| "no element found".into())
}

/// The text of an entity XML predefines, by its name.
fn predefined(name: &str) -> Result<&'static str, String> {
    match name {
        "lt" => Ok("<"),
        "gt" => Ok(">"),
        "amp" => Ok("&"),
        "apos" => Ok("'"),
        "quot" => Ok("\""),
        other => Err(format!("undefined entity &{other};")),
    }
}

fn element(start: &BytesStart) -> Result<Element, String> {
    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|e| e.to_string())?;
        let value = attribute.normalized_value(XmlVersion::Implicit1_0).map_err(|e| e.to_string())?;
        attributes.push((attribute.key.as_ref().to_string(), value.into_owned()));
    }
    Ok(Element { name: start.name().as_ref().to_string(), attributes, ..Default::default() })
}

/// Makes `done` the last child of the innermost open element, or the root when none is open.
fn close(done: Element, open: &mut [Element], root: &mut Option<Element>) -> Result<(), String> {
    match open.last_mut() {
        Some(parent) => parent.children.push(done),
        None if root.is_none() => *root = Some(done),
        None => return Err("junk after the document element".into()),
    }
    Ok(())
}

/// Adds `text` to the innermost open element's text when it has no child yet; text after a child is ElementTree's
/// tail, which nothing the gate reads needs.
fn add_text(open: &mut [Element], text: &str) {
    if let Some(e) = open.last_mut().filter(|e| e.children.is_empty()) {
        e.text.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_attributes_text_and_children() {
        let root = parse("<?xml version='1.0'?><a x='1&amp;2\n3'>t&lt;&#65;<![CDATA[<c>]]><b/><b y='2'>u</b>tail</a>")
            .unwrap();
        assert_eq!(root.get("x"), Some("1&2 3"));
        assert_eq!(root.text, "t<A<c>");
        assert_eq!(root.children_named("b").count(), 2);
        assert_eq!(root.child("b").and_then(|b| b.get("y")), None);
        assert_eq!(root.descendants("b").len(), 2);
    }

    #[test]
    fn refuses_what_is_not_well_formed() {
        for bad in ["", "<a>", "<a></b>", "<a/><b/>", "<a>&nbsp;</a>"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
