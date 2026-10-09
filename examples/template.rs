//! A template renderer forged from `schematics/tmpl.lsf`, a format-2 (LSF2)
//! sketch: template text with holes and `for` tags (lexer modes), strings
//! with `{$var}` interpolation, contextual keywords, and labelled fields.
//!
//! The renderer walks the tree by **field name** (`value`, `var`, `seq`,
//! `body`) instead of by child position, the way lower-lang's `Labeler`
//! reads a forged tree. It also saves the forged language as an image and
//! renders again from the loaded copy.
//!
//! ```text
//! cargo run --example template
//! ```

use std::collections::HashMap;

use lang_forge::syntax_lang::{Element, Node};
use lang_forge::{Kind, Language};

const PAGE: &str = "<h1>{{ title }}</h1>\n<ul>\n{% for $user in users %}  <li>{{ \"{$user} says hi\" | upper }}</li>\n{% endfor %}</ul>\n";

/// A value while rendering.
#[derive(Clone, Debug)]
enum Value {
    Text(String),
    List(Vec<Value>),
}

impl Value {
    fn text(&self) -> String {
        match self {
            Value::Text(t) => t.clone(),
            Value::List(items) => items.iter().map(Value::text).collect::<Vec<_>>().join(", "),
        }
    }
}

struct Renderer<'a> {
    lang: &'a Language,
    src: &'a str,
    vars: HashMap<String, Value>,
}

impl Renderer<'_> {
    /// The child of `node` labelled `name`, if any.
    fn field<'n>(&self, node: &'n Node<Kind>, name: &str) -> Option<&'n Element<Kind>> {
        let label = self.lang.label_id(name)?;
        node.children().find(|c| c.kind().label() == Some(label))
    }

    /// Every child of `node` labelled `name`.
    fn fields<'n>(&self, node: &'n Node<Kind>, name: &str) -> Vec<&'n Element<Kind>> {
        let label = self.lang.label_id(name);
        node.children()
            .filter(|c| label.is_some() && c.kind().label() == label)
            .collect()
    }

    fn text_of(&self, element: &Element<Kind>) -> &str {
        let span = element.span();
        &self.src[span.start().to_usize()..span.end().to_usize()]
    }

    fn items(&self, elements: &[&Element<Kind>], out: &mut String) {
        for element in elements {
            match element {
                Element::Token(_) => out.push_str(self.text_of(element)),
                Element::Node(node) => self.item(node, out),
            }
        }
    }

    fn item(&self, node: &Node<Kind>, out: &mut String) {
        match self.lang.kind_name(*node.kind()) {
            "hole" => {
                if let Some(Element::Node(value)) = self.field(node, "value") {
                    out.push_str(&self.eval(value).text());
                }
            }
            "for_tag" => {
                let var = self
                    .field(node, "var")
                    .map(|v| self.text_of(v).to_owned())
                    .unwrap_or_default();
                let Some(Element::Node(seq)) = self.field(node, "seq") else {
                    return;
                };
                let Value::List(items) = self.eval(seq) else {
                    return;
                };
                let body = self.fields(node, "body");
                for item in items {
                    let mut inner = Renderer {
                        lang: self.lang,
                        src: self.src,
                        vars: self.vars.clone(),
                    };
                    let _ = inner.vars.insert(var.clone(), item);
                    inner.items(&body, out);
                }
            }
            _ => {}
        }
    }

    /// Evaluates an `expr` node (or any node inside one).
    fn eval(&self, node: &Node<Kind>) -> Value {
        let kind = self.lang.kind_name(*node.kind());
        match kind {
            "expr" => match node.children().find(|c| !matches!(c, Element::Token(t) if lang_forge::syntax_lang::TokenKind::is_trivia(t.kind()))) {
                Some(Element::Node(inner)) => self.eval(inner),
                Some(token) => self.atom(token),
                None => Value::Text(String::new()),
            },
            "filter" => {
                let value = self.operand(node, "lhs").text();
                let filter = self.field(node, "rhs").map(|f| self.text_of(f)).unwrap_or("");
                Value::Text(if filter == "upper" { value.to_uppercase() } else { value })
            }
            "concat" => Value::Text(self.operand(node, "lhs").text() + &self.operand(node, "rhs").text()),
            "STR" => {
                // Text parts as written; `{$var}` holes evaluated.
                let mut out = String::new();
                for part in self.fields(node, "parts") {
                    match part {
                        Element::Node(hole) => {
                            if let Some(Element::Node(value)) = self.field(hole, "value") {
                                out.push_str(&self.eval(value).text());
                            }
                        }
                        Element::Token(_) => out.push_str(self.text_of(part)),
                    }
                }
                Value::Text(out)
            }
            _ => Value::Text(String::new()),
        }
    }

    fn operand(&self, node: &Node<Kind>, name: &str) -> Value {
        match self.field(node, name) {
            Some(Element::Node(n)) => self.eval(n),
            Some(token) => self.atom(token),
            None => Value::Text(String::new()),
        }
    }

    fn atom(&self, element: &Element<Kind>) -> Value {
        let text = self.text_of(element);
        self.vars
            .get(text)
            .cloned()
            .unwrap_or_else(|| Value::Text(text.to_owned()))
    }
}

fn main() -> Result<(), lang_forge::Error> {
    let lang = Language::from_lsf(include_str!("schematics/tmpl.lsf"))?;
    println!(
        "forged {} (format {}), {} kinds",
        lang.display_name(),
        lang.format(),
        lang.kind_count()
    );
    for kind in ["hole", "for_tag"] {
        let fields: Vec<String> = lang
            .fields(lang.kind(kind).expect("a rule"))
            .map(|f| format!("{}: {:?}", f.name(), f.cardinality()))
            .collect();
        println!("  {kind} fields: {}", fields.join(", "));
    }

    let mut vars = HashMap::new();
    let _ = vars.insert("title".to_owned(), Value::Text("Users".to_owned()));
    let _ = vars.insert(
        "users".to_owned(),
        Value::List(vec![
            Value::Text("ada".to_owned()),
            Value::Text("grace".to_owned()),
        ]),
    );

    // Render with the forged language, then with one loaded from its image.
    let image = lang.to_image();
    let loaded = Language::from_image(&image).expect("its own image loads");
    for (name, lang) in [("forged", &lang), ("from image", &loaded)] {
        let parse = lang.parse(PAGE);
        for d in parse.diagnostics() {
            println!("{name}: {}", d.message());
        }
        let renderer = Renderer {
            lang,
            src: PAGE,
            vars: vars.clone(),
        };
        let mut out = String::new();
        let items = renderer.fields(parse.tree(), "items");
        renderer.items(&items, &mut out);
        println!("--- {name} ({} image bytes) ---\n{out}", image.len());
    }
    Ok(())
}
