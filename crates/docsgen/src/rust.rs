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

/// The early-return guards of a function, in the order they are checked.
///
/// A precedence ladder is not prose about a function, it is the function: a run
/// of `if <cond> { return <verdict> }` at the top of a body, where the first
/// one that answers is the answer. Reading them back gives a diagram of the
/// order that cannot disagree with the code, which the module doc's hand-kept
/// numbered list can and does.
///
/// `name` is the condition as source text; `detail` is the returned value when
/// it is simple enough to name. A guard whose body does not return is not a
/// guard and is skipped -- it is a step in the function, not a rung.
///
/// One level of nesting is flattened: `if !forced { if a {..} if b {..} }` in
/// `Approvals::decide` is three rungs, not one, and drawing it as one would
/// lose the order this exists to show.
pub fn fn_guards(file: &syn::File, name: &str) -> Result<Vec<Item>> {
    let body =
        fn_body(&file.items, name).with_context(|| format!("no `fn {name}` in this file"))?;
    let mut out = Vec::new();
    collect_guards(&body, &mut out, true);
    if out.is_empty() {
        bail!("`fn {name}` has no early-return guards to draw");
    }
    Ok(out)
}

fn collect_guards(stmts: &[syn::Stmt], out: &mut Vec<Item>, descend: bool) {
    for stmt in stmts {
        match stmt {
            // `let Some(x) = e else { return … }` is the same rung as an `if`
            // that returns, and it was invisible here until 2026-09-04. The
            // danger was not the shape being missed but being missed *among*
            // `if` guards: a function mixing both drew a partial ladder, which
            // is a confident picture of the wrong order. Nothing shipped had
            // that mix -- checked against every function the ladders read --
            // and the next one would not have been so lucky.
            syn::Stmt::Local(local) => {
                let Some(init) = &local.init else { continue };
                let Some((_, diverge)) = &init.diverge else {
                    continue;
                };
                let syn::Expr::Block(b) = diverge.as_ref() else {
                    continue;
                };
                let Some(verdict) = block_returns(&b.block) else {
                    continue;
                };
                out.push(Item {
                    name: cond_text(&init.expr),
                    doc: String::new(),
                    detail: verdict,
                });
            }
            syn::Stmt::Expr(syn::Expr::If(iff), _) => {
                if let Some(verdict) = block_returns(&iff.then_branch) {
                    out.push(Item {
                        name: cond_text(&iff.cond),
                        doc: String::new(),
                        detail: verdict,
                    });
                } else if descend {
                    // A guard whose body is itself guards -- `if !forced`.
                    collect_guards(&iff.then_branch.stmts, out, false);
                }
            }
            _ => {}
        }
    }
}

/// The value an early-return block returns, when the block is one `return`.
///
/// `None` for a block that does other work first: that is a branch rather than
/// a rung, and putting it on the ladder would claim an ordering the function
/// does not have.
fn block_returns(b: &syn::Block) -> Option<String> {
    let mut found = None;
    for stmt in &b.stmts {
        if let syn::Stmt::Expr(syn::Expr::Return(r), _) = stmt {
            found = Some(r.expr.as_ref().map(|e| short_call(e)).unwrap_or_default());
        }
    }
    found
}

/// A condition as short readable text.
fn cond_text(e: &syn::Expr) -> String {
    match e {
        syn::Expr::Binary(b) => format!(
            "{} {} {}",
            cond_text(&b.left),
            match b.op {
                syn::BinOp::Eq(_) => "==",
                syn::BinOp::Ne(_) => "!=",
                syn::BinOp::And(_) => "&&",
                syn::BinOp::Or(_) => "||",
                _ => "?",
            },
            cond_text(&b.right)
        ),
        syn::Expr::Unary(u) => format!("!{}", cond_text(&u.expr)),
        syn::Expr::MethodCall(m) => {
            // `self` adds nothing to a label inside its own impl, and the
            // receiver is what tells a reader which thing is being asked --
            // `EXEMPT.contains()` rather than `.contains()`.
            let recv = cond_text(&m.receiver);
            if recv.is_empty() || recv == "self" {
                format!("{}()", m.method)
            } else {
                format!("{recv}.{}()", m.method)
            }
        }
        syn::Expr::Field(f) => match &f.member {
            syn::Member::Named(n) => n.to_string(),
            syn::Member::Unnamed(i) => i.index.to_string(),
        },
        syn::Expr::Let(l) => cond_text(&l.expr),
        // `.await` is transparent here: the label wants the call, not the fact
        // that it is asynchronous. Without this the egress rung of
        // `Approvals::decide` renders as an empty box.
        syn::Expr::Await(a) => cond_text(&a.base),
        syn::Expr::Try(t) => cond_text(&t.expr),
        syn::Expr::Path(p) => path_tail(&p.path),
        // Arguments are kept for a one-argument call, because they carry the
        // fact: `rule == Some(Deny)` and `rule == Some(Allow)` are two
        // different rungs and `rule == Some` is the same box twice.
        syn::Expr::Call(c) => {
            let f = cond_text(&c.func);
            match c.args.len() {
                1 => {
                    let a = cond_text(&c.args[0]);
                    if a.is_empty() {
                        format!("{f}()")
                    } else {
                        format!("{f}({a})")
                    }
                }
                _ => format!("{f}()"),
            }
        }
        syn::Expr::Paren(p) => cond_text(&p.expr),
        syn::Expr::Reference(r) => cond_text(&r.expr),
        syn::Expr::Lit(l) => match &l.lit {
            syn::Lit::Str(s) => format!("{:?}", s.value()),
            syn::Lit::Int(i) => i.base10_digits().to_string(),
            syn::Lit::Bool(b) => b.value.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The last segment of a path, or the last two when the leading one is a type
/// that carries meaning: `Decision::Deny` rather than `Deny`.
fn path_tail(p: &syn::Path) -> String {
    let n = p.segments.len();
    if n >= 2 {
        let head = &p.segments[n - 2].ident;
        let tail = &p.segments[n - 1].ident;
        let head_s = head.to_string();
        // A module path adds noise; a type path adds the fact. Types are
        // capitalised in this codebase, which is the only signal available
        // without resolving names.
        if head_s.chars().next().is_some_and(char::is_uppercase) {
            return format!("{head_s}::{tail}");
        }
        return tail.to_string();
    }
    p.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default()
}

/// A call or path reduced to its last recognisable name.
fn short_call(e: &syn::Expr) -> String {
    match e {
        syn::Expr::Call(c) => {
            let f = cond_text(&c.func);
            if f.is_empty() {
                String::new()
            } else {
                f
            }
        }
        syn::Expr::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        syn::Expr::MethodCall(m) => m.method.to_string(),
        _ => String::new(),
    }
}

fn fn_body(items: &[syn::Item], name: &str) -> Option<Vec<syn::Stmt>> {
    for item in items {
        match item {
            syn::Item::Fn(f) if f.sig.ident == name => return Some(f.block.stmts.clone()),
            syn::Item::Impl(i) => {
                for it in &i.items {
                    if let syn::ImplItem::Fn(f) = it {
                        if f.sig.ident == name {
                            return Some(f.block.stmts.clone());
                        }
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    if let Some(found) = fn_body(inner, name) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
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

    /// **If this breaks:** a precedence ladder is drawn in an order the
    /// function does not check in, which is the one thing such a diagram
    /// exists to state.
    #[test]
    fn guards_come_back_in_the_order_the_function_checks_them() {
        let f = file(
            "impl A {\n\
             fn decide(&self) -> V {\n\
             let rule = look();\n\
             if rule == Deny { return V::Deny; }\n\
             if self.gate == SkipAll { return V::Allow; }\n\
             V::Ask\n\
             }\n}",
        );
        let g = fn_guards(&f, "decide").expect("decide has guards");
        assert_eq!(g.len(), 2, "{g:?}");
        assert_eq!(g[0].name, "rule == Deny");
        assert_eq!(g[1].name, "gate == SkipAll");
    }

    /// **If this breaks:** a `let … else { return … }` stops counting as a
    /// rung, and a function that mixes it with `if` guards draws a ladder that
    /// is confidently short -- the worst outcome for a picture whose only claim
    /// is an order.
    ///
    /// Reported by a reviewer on 2026-09-04, which is also when it was checked
    /// against every function the shipped ladders read: none mixed the two, so
    /// nothing drawn was partial. That was luck rather than design.
    #[test]
    fn a_let_else_that_returns_is_a_rung_like_any_other() {
        let f = file(
            "fn d() -> V {
             let Some(a) = first() else { return V::A; };
             if b { return V::B; }
             let Ok(c) = second() else { return V::C; };
             V::D
}",
        );
        let g = fn_guards(&f, "d").expect("guards");
        assert_eq!(
            g.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["first()", "b", "second()"],
            "the two let-else rungs sit in source order around the if: {g:?}"
        );
    }

    /// **If this breaks:** an ordinary `let` with no `else` is counted as a
    /// guard, and the ladder grows rungs the function does not check.
    #[test]
    fn a_plain_let_binding_is_not_a_rung() {
        let f = file(
            "fn d() -> V {
 let x = work();
 if b { return V::B; }
 V::C
}",
        );
        let g = fn_guards(&f, "d").expect("guards");
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].name, "b");
    }

    /// **If this breaks:** a nested block of guards collapses to one rung and
    /// the ladder loses the steps inside it. `Approvals::decide` puts three
    /// rungs inside `if !forced`.
    #[test]
    fn one_level_of_nested_guards_is_flattened_into_the_ladder() {
        let f = file(
            "fn d() -> V {\n\
             if a { return V::A; }\n\
             if !forced {\n\
             if b { return V::B; }\n\
             if c { return V::C; }\n\
             }\n\
             V::D\n}",
        );
        let g = fn_guards(&f, "d").expect("guards");
        assert_eq!(
            g.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"],
            "{g:?}"
        );
    }

    /// **If this breaks:** an `if` that does work rather than answering is put
    /// on the ladder, claiming an ordering the function does not have.
    #[test]
    fn a_branch_that_does_not_return_is_not_a_rung() {
        let f = file("fn d() -> V {\n if a { log(); }\n if b { return V::B; }\n V::C\n}");
        let g = fn_guards(&f, "d").expect("guards");
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].name, "b");
    }

    /// **If this breaks:** a function with no ladder in it is drawn as an
    /// empty one rather than refusing.
    #[test]
    fn a_function_without_guards_is_an_error() {
        let f = file("fn d() -> V { V::C }");
        assert!(fn_guards(&f, "d").is_err());
        assert!(fn_guards(&f, "absent").is_err());
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
