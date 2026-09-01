//! Reading facts out of Rust source, so a diagram can be drawn from the thing
//! it describes.
//!
//! Every diagram in `docs/` names a source file. Until now that was a citation
//! a reader had to take on trust: the picture was drawn by somebody who had
//! read the file, and nothing re-read it afterwards. These functions are what
//! turn the citation into a lookup.
//!
//! # Parsed, not matched
//!
//! `syn` rather than regexes. A variant behind a `#[cfg]`, a doc comment
//! containing a brace, a field whose type spans two lines, a `match` arm with a
//! guard -- each defeats a pattern, and none defeats a parser. The failure a
//! regex produces here is also the worst kind: it finds three of four variants
//! and draws a confident picture of three.
//!
//! # What each function promises
//!
//! A `None` or an empty result means the item was not found, and every caller
//! treats that as an error rather than drawing an empty diagram. A diagram with
//! no boxes still renders, and still looks like a diagram.

use anyhow::{bail, Context, Result};
use std::path::Path;

/// One named thing read out of source: an enum variant, a struct field, a
/// match arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    /// The first sentence of its doc comment, empty when it has none.
    pub doc: String,
    /// A type, a discriminant, or whatever else the reader of this item
    /// wanted alongside the name. Empty when there is nothing.
    pub detail: String,
}

/// Parse a file once. Callers that want two things from one file should parse
/// once and pass the result around.
pub fn parse(path: &Path) -> Result<syn::File> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("{} is a diagram's source", path.display()))?;
    syn::parse_file(&text).with_context(|| format!("{} did not parse", path.display()))
}

/// The variants of a named enum, in declaration order.
///
/// Declaration order because that is the order the author chose, and for the
/// enums drawn here it is meaningful -- a readiness ladder and a set of failure
/// classes both read differently sorted alphabetically.
pub fn enum_variants(file: &syn::File, name: &str) -> Result<Vec<Item>> {
    for item in &file.items {
        let syn::Item::Enum(e) = item else { continue };
        if e.ident != name {
            continue;
        }
        return Ok(e
            .variants
            .iter()
            .map(|v| Item {
                name: v.ident.to_string(),
                doc: first_doc_sentence(&v.attrs),
                detail: match &v.fields {
                    syn::Fields::Unit => String::new(),
                    syn::Fields::Named(f) => format!("{} fields", f.named.len()),
                    syn::Fields::Unnamed(f) => format!("{} values", f.unnamed.len()),
                },
            })
            .collect());
    }
    bail!("no `enum {name}` in this file")
}

/// The named fields of a struct, in declaration order.
pub fn struct_fields(file: &syn::File, name: &str) -> Result<Vec<Item>> {
    for item in &file.items {
        let syn::Item::Struct(s) = item else { continue };
        if s.ident != name {
            continue;
        }
        let syn::Fields::Named(named) = &s.fields else {
            bail!("`struct {name}` has no named fields");
        };
        return Ok(named
            .named
            .iter()
            .map(|f| Item {
                name: f
                    .ident
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                doc: first_doc_sentence(&f.attrs),
                detail: type_name(&f.ty),
            })
            .collect());
    }
    bail!("no `struct {name}` in this file")
}

/// The value of a `const`, as written.
///
/// Returned as source text rather than evaluated. A diagram showing a clamp of
/// `28..=40` wants those numbers; running the expression would need a compiler
/// and would produce the same answer.
pub fn const_value(file: &syn::File, name: &str) -> Result<String> {
    fn find(items: &[syn::Item], name: &str) -> Option<String> {
        for item in items {
            match item {
                syn::Item::Const(c) if c.ident == name => {
                    return Some(expr_text(&c.expr));
                }
                syn::Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        if let Some(found) = find(inner, name) {
                            return Some(found);
                        }
                    }
                }
                syn::Item::Impl(i) => {
                    for it in &i.items {
                        if let syn::ImplItem::Const(c) = it {
                            if c.ident == name {
                                return Some(expr_text(&c.expr));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
    find(&file.items, name).with_context(|| format!("no `const {name}` in this file"))
}

/// Whether a function of this name exists anywhere in the file, including
/// inside an `impl`.
///
/// Used by diagrams that name a function as a step: if the function is gone,
/// the step is wrong and the diagram should fail rather than draw it.
pub fn has_fn(file: &syn::File, name: &str) -> bool {
    fn walk(items: &[syn::Item], name: &str) -> bool {
        items.iter().any(|item| match item {
            syn::Item::Fn(f) => f.sig.ident == name,
            syn::Item::Impl(i) => i.items.iter().any(|it| match it {
                syn::ImplItem::Fn(f) => f.sig.ident == name,
                _ => false,
            }),
            syn::Item::Trait(t) => t.items.iter().any(|it| match it {
                syn::TraitItem::Fn(f) => f.sig.ident == name,
                _ => false,
            }),
            syn::Item::Mod(m) => m
                .content
                .as_ref()
                .is_some_and(|(_, inner)| walk(inner, name)),
            _ => false,
        })
    }
    walk(&file.items, name)
}

/// The first sentence of an item's `///` documentation.
///
/// First sentence rather than the whole comment: these become labels beside a
/// box, and a paragraph beside a box is a paragraph nobody reads. Markdown
/// emphasis is stripped, because the label lands in SVG text where `**` is two
/// asterisks.
fn first_doc_sentence(attrs: &[syn::Attribute]) -> String {
    let mut text = String::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        let syn::Meta::NameValue(nv) = &attr.meta else {
            continue;
        };
        let syn::Expr::Lit(lit) = &nv.value else {
            continue;
        };
        let syn::Lit::Str(s) = &lit.lit else { continue };
        let line = s.value();
        let line = line.trim();
        if line.is_empty() && !text.is_empty() {
            break; // A blank line ends the first paragraph.
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
        if text.contains(". ") || text.ends_with('.') {
            break;
        }
    }
    let text = text.replace("**", "").replace('`', "");
    match text.find(". ") {
        Some(i) => text[..=i].trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// A type as short source text: `Option<String>` rather than a token tree.
fn type_name(ty: &syn::Type) -> String {
    use quote_min::ToText as _;
    ty.to_text()
}

fn expr_text(e: &syn::Expr) -> String {
    use quote_min::ToText as _;
    e.to_text()
}

/// Rendering a `syn` node back to text without pulling in `quote`.
///
/// `syn`'s own `Debug` is a token tree, which is unreadable in a diagram
/// label. This is the smallest thing that turns a type or a literal back into
/// something a person recognises.
mod quote_min {
    pub trait ToText {
        fn to_text(&self) -> String;
    }

    impl ToText for syn::Type {
        fn to_text(&self) -> String {
            match self {
                syn::Type::Path(p) => p
                    .path
                    .segments
                    .iter()
                    .map(|s| {
                        let args = match &s.arguments {
                            syn::PathArguments::AngleBracketed(a) => {
                                let inner: Vec<String> = a
                                    .args
                                    .iter()
                                    .map(|g| match g {
                                        syn::GenericArgument::Type(t) => t.to_text(),
                                        _ => String::new(),
                                    })
                                    .filter(|s| !s.is_empty())
                                    .collect();
                                if inner.is_empty() {
                                    String::new()
                                } else {
                                    format!("<{}>", inner.join(", "))
                                }
                            }
                            _ => String::new(),
                        };
                        format!("{}{args}", s.ident)
                    })
                    .collect::<Vec<_>>()
                    .join("::"),
                syn::Type::Reference(r) => format!("&{}", r.elem.to_text()),
                syn::Type::Slice(s) => format!("[{}]", s.elem.to_text()),
                syn::Type::Tuple(t) if t.elems.is_empty() => "()".into(),
                _ => String::new(),
            }
        }
    }

    impl ToText for syn::Expr {
        fn to_text(&self) -> String {
            match self {
                syn::Expr::Lit(l) => match &l.lit {
                    syn::Lit::Int(i) => i.base10_digits().to_string(),
                    syn::Lit::Float(f) => f.base10_digits().to_string(),
                    syn::Lit::Str(s) => s.value(),
                    syn::Lit::Bool(b) => b.value.to_string(),
                    _ => String::new(),
                },
                syn::Expr::Unary(u) => format!("-{}", u.expr.to_text()),
                syn::Expr::Binary(b) => format!(
                    "{} {} {}",
                    b.left.to_text(),
                    match b.op {
                        syn::BinOp::Mul(_) => "*",
                        syn::BinOp::Div(_) => "/",
                        syn::BinOp::Add(_) => "+",
                        syn::BinOp::Sub(_) => "-",
                        _ => "?",
                    },
                    b.right.to_text()
                ),
                syn::Expr::Path(p) => p
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(src: &str) -> syn::File {
        syn::parse_file(src).expect("the fixture parses")
    }

    /// **If this breaks:** a diagram of an enum shows the wrong set of states,
    /// which is the failure a regex would produce silently.
    #[test]
    fn variants_come_back_in_declaration_order_with_their_first_sentence() {
        let f = file(
            "/// outer\npub enum R {\n\
             /// Nothing heard yet. More words after the stop.\n Handshaking,\n\
             /// Work in progress.\n Indexing,\n Ready,\n}",
        );
        let v = enum_variants(&f, "R").expect("R exists");
        assert_eq!(
            v.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["Handshaking", "Indexing", "Ready"]
        );
        assert_eq!(v[0].doc, "Nothing heard yet.");
        assert_eq!(v[2].doc, "", "an undocumented variant has no sentence");
    }

    /// **If this breaks:** a diagram is drawn for an item that no longer
    /// exists, from whatever the lookup happened to return.
    #[test]
    fn a_missing_item_is_an_error_rather_than_an_empty_diagram() {
        let f = file("pub enum R { A }");
        assert!(enum_variants(&f, "Nope").is_err());
        assert!(struct_fields(&f, "Nope").is_err());
        assert!(const_value(&f, "NOPE").is_err());
    }

    /// **If this breaks:** a variant hidden behind an attribute is dropped, and
    /// the picture is confidently short by one. This is the case a regex over
    /// `^\s*\w+,` gets wrong.
    #[test]
    fn an_attribute_on_a_variant_does_not_hide_it() {
        let f = file("pub enum R {\n #[serde(rename = \"a\")]\n A,\n #[cfg(unix)]\n B,\n}");
        let v = enum_variants(&f, "R").expect("R exists");
        assert_eq!(v.len(), 2, "{v:?}");
    }

    /// **If this breaks:** a struct diagram labels a field with a token tree.
    #[test]
    fn a_field_carries_its_type_in_a_form_a_person_reads() {
        let f = file("pub struct S {\n pub a: Option<String>,\n pub b: u32,\n}");
        let s = struct_fields(&f, "S").expect("S exists");
        assert_eq!(s[0].detail, "Option<String>");
        assert_eq!(s[1].detail, "u32");
    }

    /// **If this breaks:** a diagram naming a constant shows a stale number,
    /// which is the class of error this whole crate exists to end.
    #[test]
    fn a_const_comes_back_as_written_including_an_expression() {
        let f = file("const A: u16 = 40;\nconst B: f32 = 22.4;\nimpl X { const C: u8 = 3; }");
        assert_eq!(const_value(&f, "A").expect("A"), "40");
        assert_eq!(const_value(&f, "B").expect("B"), "22.4");
        assert_eq!(const_value(&f, "C").expect("C in an impl"), "3");
    }

    /// **If this breaks:** a diagram names a step whose function was renamed
    /// or deleted, and nothing notices.
    #[test]
    fn a_function_is_found_wherever_it_is_declared() {
        let f = file("fn top() {}\nimpl X { fn inner(&self) {} }\nmod m { fn nested() {} }");
        assert!(has_fn(&f, "top"));
        assert!(has_fn(&f, "inner"));
        assert!(has_fn(&f, "nested"));
        assert!(!has_fn(&f, "absent"));
    }
}
