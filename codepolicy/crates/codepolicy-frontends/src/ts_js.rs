//! TypeScript / JavaScript frontend (proposal §6.1) built on Tree-sitter.
//!
//! Walks the concrete syntax tree once and projects the v0 node kinds down to
//! canonical events: Import, Call, TypeDecl, FunctionDecl, EnvAccess,
//! StringLiteral, Comment (plus a per-file File event).

use camino::{Utf8Path, Utf8PathBuf};
use codepolicy_events::{Event, EventKind, Interner, Language, Span, Token, TokenStream, NO_JMP};
use serde_json::{json, Value};
use std::sync::Arc;
use tree_sitter::{Language as TsLanguage, Node, Parser};

use super::{Extracted, LanguageFrontend, SourceFile};

pub struct TsJsFrontend;

impl LanguageFrontend for TsJsFrontend {
    fn name(&self) -> &'static str {
        "ts_js"
    }

    fn supports_file(&self, path: &Utf8Path) -> bool {
        matches!(
            path.extension(),
            Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
        )
    }

    fn extract(&self, file: &SourceFile<'_>, want_tokens: bool) -> anyhow::Result<Extracted> {
        let ext = file.path.extension().unwrap_or("");
        let (language, ts_lang) = select_language(ext);

        let mut parser = Parser::new();
        parser
            .set_language(&ts_lang)
            .map_err(|e| anyhow::anyhow!("failed to load grammar: {e}"))?;
        let src = file.text.as_bytes();
        let tree = parser
            .parse(src, None)
            .ok_or_else(|| anyhow::anyhow!("parser returned no tree for {}", file.path))?;

        // One parse feeds both streams: canonical events and the token stream.
        let mut events = Vec::new();
        let mut tb = TokenBuilder::default();
        let root = tree.root_node();
        let file_arc: Arc<Utf8PathBuf> = Arc::new(file.path.clone());
        events.push(
            Event::new(EventKind::File, language, file_arc.clone(), span_of(root))
                .with("path", json!(file.path.as_str())),
        );

        let ctx = Ctx {
            src,
            language,
            file: file_arc.clone(),
            want_tokens,
        };
        let mut scope_counter = 0usize;
        walk(
            root,
            &ctx,
            &mut events,
            &mut tb,
            &mut scope_counter,
            &Frame::default(),
        );
        let tokens = if want_tokens {
            tb.finish_jmp();
            Some(TokenStream {
                file: file_arc,
                language,
                interner: tb.interner,
                tokens: tb.tokens,
            })
        } else {
            None
        };
        Ok(Extracted { tokens, events })
    }
}

fn select_language(ext: &str) -> (Language, TsLanguage) {
    match ext {
        "tsx" => (
            Language::Typescript,
            tree_sitter_typescript::LANGUAGE_TSX.into(),
        ),
        "ts" | "mts" | "cts" => (
            Language::Typescript,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        // .js/.jsx/.mjs/.cjs and anything else routed here.
        _ => (
            Language::Javascript,
            tree_sitter_javascript::LANGUAGE.into(),
        ),
    }
}

struct Ctx<'a> {
    src: &'a [u8],
    language: Language,
    file: Arc<Utf8PathBuf>,
    want_tokens: bool,
}

impl<'a> Ctx<'a> {
    fn event(&self, kind: EventKind, node: Node) -> Event {
        Event::new(kind, self.language, self.file.clone(), span_of(node))
    }
}

fn span_of(n: Node) -> Span {
    let s = n.start_position();
    let e = n.end_position();
    Span {
        start_byte: n.start_byte(),
        end_byte: n.end_byte(),
        // Tree-sitter rows/columns are 0-based; canonical spans are 1-based.
        start_line: s.row + 1,
        start_col: s.column + 1,
        end_line: e.row + 1,
        end_col: e.column + 1,
    }
}

fn text<'a>(n: Node, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

/// Truncate to at most `max` characters (for generic Token `text`, which can be
/// a whole subtree's source).
fn capped(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// Inner text of a `string` node (the `string_fragment`), else the literal
/// with surrounding quotes stripped.
fn string_value(n: Node, src: &[u8]) -> String {
    let mut cur = n.walk();
    for ch in n.named_children(&mut cur) {
        if ch.kind() == "string_fragment" {
            return text(ch, src).to_string();
        }
    }
    text(n, src)
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_string()
}

/// Normalize a Tree-sitter argument node kind to a friendly, language-neutral
/// **syntactic** kind (the literal/expression form — not a static type).
fn normalize_arg_kind(k: &str) -> &'static str {
    match k {
        "string" => "string",
        "template_string" => "template",
        "number" => "number",
        "true" | "false" => "bool",
        "identifier" | "shorthand_property_identifier" | "property_identifier" => "identifier",
        "call_expression" => "call",
        "member_expression" | "subscript_expression" => "member",
        "object" => "object",
        "array" => "array",
        "arrow_function" | "function" | "function_expression" | "generator_function"
        | "function_declaration" => "function",
        "regex" => "regex",
        "null" => "null",
        "undefined" => "undefined",
        _ => "other",
    }
}

/// Structural context (proposal §8.8): nesting depths and the enclosing
/// function, accumulated as the walk descends. Mirrors Cobra's `.curly`/
/// `.round`/`.bracket`/`.fct` fields.
#[derive(Clone, Default)]
struct Frame {
    curly: u32,
    round: u32,
    bracket: u32,
    function: Option<String>,
}

fn walk(
    node: Node,
    ctx: &Ctx,
    events: &mut Vec<Event>,
    tokens: &mut TokenBuilder,
    scope_counter: &mut usize,
    frame: &Frame,
) {
    // Canonical (structured) event — the optional overlay.
    if let Some(mut ev) = event_for(node, ctx) {
        attach_structural(&mut ev, frame);
        events.push(ev);
    }
    // Universal token layer: a compact Token for every node — named constructs
    // (`switch_statement`) AND bare symbol literals (`switch`, `+`, `{`).
    if ctx.want_tokens {
        tokens.push(node, ctx.src, frame);
    }
    // Paired ScopeStart/ScopeEnd for each `{}` block (canonical structure).
    if matches!(node.kind(), "statement_block" | "class_body") {
        let sid = *scope_counter;
        *scope_counter += 1;
        emit_scope(node, ctx, sid, frame, events);
    }
    let child = child_frame(node, ctx.src, frame);
    let mut cur = node.walk();
    for c in node.children(&mut cur) {
        walk(c, ctx, events, tokens, scope_counter, &child);
    }
}

/// Accumulates the compact token stream (interner + `Vec<Token>`).
#[derive(Default)]
struct TokenBuilder {
    interner: Interner,
    tokens: Vec<Token>,
}

impl TokenBuilder {
    fn push(&mut self, node: Node, src: &[u8], frame: &Frame) {
        let kind = self.interner.intern(node.kind());
        let text = self.interner.intern(&capped(text(node, src), 120));
        let func = self.interner.intern(frame.function.as_deref().unwrap_or(""));
        let s = node.start_position();
        let e = node.end_position();
        self.tokens.push(Token {
            kind,
            text,
            func,
            named: node.is_named(),
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            start_line: (s.row + 1) as u32,
            start_col: (s.column + 1) as u32,
            end_line: (e.row + 1) as u32,
            end_col: (e.column + 1) as u32,
            curly: frame.curly.min(u16::MAX as u32) as u16,
            round: frame.round.min(u16::MAX as u32) as u16,
            bracket: frame.bracket.min(u16::MAX as u32) as u16,
            jmp: NO_JMP,
        });
    }

    /// Cobra-style delimiter links: pair `()`/`{}`/`[]` and set `jmp` both ways.
    fn finish_jmp(&mut self) {
        let (lp, rp) = (self.interner.intern("("), self.interner.intern(")"));
        let (lb, rb) = (self.interner.intern("{"), self.interner.intern("}"));
        let (ls, rs) = (self.interner.intern("["), self.interner.intern("]"));
        let mut stack: Vec<usize> = Vec::new();
        for i in 0..self.tokens.len() {
            let k = self.tokens[i].kind;
            if k == lp || k == lb || k == ls {
                stack.push(i);
            } else if k == rp || k == rb || k == rs {
                let want = if k == rp {
                    lp
                } else if k == rb {
                    lb
                } else {
                    ls
                };
                if let Some(&top) = stack.last() {
                    if self.tokens[top].kind == want {
                        stack.pop();
                        self.tokens[top].jmp = i as u32;
                        self.tokens[i].jmp = top as u32;
                    }
                }
            }
        }
    }
}

/// Attach reserved structural attributes (§8.8) to an event.
fn attach_structural(ev: &mut Event, frame: &Frame) {
    ev.attrs.insert("curly_depth".into(), json!(frame.curly));
    ev.attrs.insert("round_depth".into(), json!(frame.round));
    ev.attrs.insert("bracket_depth".into(), json!(frame.bracket));
    let lines = ev.span.end_line.saturating_sub(ev.span.start_line) + 1;
    ev.attrs.insert("range_lines".into(), json!(lines));
    ev.attrs.insert(
        "text_len".into(),
        json!(ev.span.end_byte.saturating_sub(ev.span.start_byte)),
    );
    if let Some(f) = &frame.function {
        ev.attrs.insert("function".into(), json!(f));
    }
}

/// Compute the frame for a node's children: increment the relevant nesting
/// depth and update the enclosing-function name.
fn child_frame(node: Node, src: &[u8], frame: &Frame) -> Frame {
    let mut f = frame.clone();
    match node.kind() {
        // Code scopes only (object literals do not count toward curly_depth).
        "statement_block" | "class_body" => f.curly += 1,
        "arguments" | "formal_parameters" | "parenthesized_expression" => f.round += 1,
        "array" | "array_pattern" => f.bracket += 1,
        _ => {}
    }
    if matches!(
        node.kind(),
        "function_declaration"
            | "function_signature"
            | "generator_function_declaration"
            | "method_definition"
    ) {
        if let Some(name) = node.child_by_field_name("name") {
            f.function = Some(text(name, src).to_string());
        }
    }
    f
}

fn emit_scope(node: Node, ctx: &Ctx, sid: usize, frame: &Frame, out: &mut Vec<Event>) {
    let child_count = node.child_count() as u32;
    let open = node.child(0);
    let close = node.child(child_count.saturating_sub(1));
    let start_span = open.map(span_of).unwrap_or_else(|| span_of(node));
    let end_span = close.map(span_of).unwrap_or_else(|| span_of(node));
    let scope_kind = node.kind();
    out.push(
        Event::new(EventKind::ScopeStart, ctx.language, ctx.file.clone(), start_span)
            .with("scope_id", json!(sid))
            .with("scope_kind", json!(scope_kind))
            .with("curly_depth", json!(frame.curly)),
    );
    out.push(
        Event::new(EventKind::ScopeEnd, ctx.language, ctx.file.clone(), end_span)
            .with("scope_id", json!(sid))
            .with("scope_kind", json!(scope_kind))
            .with("curly_depth", json!(frame.curly)),
    );
}

fn event_for(node: Node, ctx: &Ctx) -> Option<Event> {
    let src = ctx.src;
    match node.kind() {
        "import_statement" => {
            let source = node
                .child_by_field_name("source")
                .map(|s| string_value(s, src))
                .unwrap_or_default();
            let mut symbols: Vec<String> = Vec::new();
            let mut cur = node.walk();
            for ch in node.named_children(&mut cur) {
                if ch.kind() == "import_clause" {
                    collect_import_symbols(ch, src, &mut symbols);
                }
            }
            Some(
                ctx.event(EventKind::Import, node)
                    .with("source", json!(source))
                    .with("symbols", Value::Array(symbols.into_iter().map(Value::String).collect())),
            )
        }

        "call_expression" => {
            let func = node.child_by_field_name("function");
            let (name, callee, receiver) = match func {
                Some(f) if f.kind() == "member_expression" => {
                    let prop = f
                        .child_by_field_name("property")
                        .map(|p| text(p, src).to_string())
                        .unwrap_or_default();
                    // The object the method is called on, e.g. `el` in `el.on(...)`.
                    let obj = f
                        .child_by_field_name("object")
                        .map(|o| text(o, src).to_string())
                        .unwrap_or_default();
                    (prop, text(f, src).to_string(), obj)
                }
                Some(f) => (text(f, src).to_string(), text(f, src).to_string(), String::new()),
                None => (String::new(), String::new(), String::new()),
            };
            let mut string_args: Vec<String> = Vec::new();
            // Positional args: (value, syntactic kind) in source order — all args,
            // not just string literals (proposal §8.8 argument capture).
            let mut positional: Vec<(String, &'static str)> = Vec::new();
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cur = args.walk();
                for a in args.named_children(&mut cur) {
                    let value = if a.kind() == "string" {
                        string_value(a, src)
                    } else {
                        text(a, src).to_string()
                    };
                    if a.kind() == "string" {
                        string_args.push(value.clone());
                    }
                    positional.push((value, normalize_arg_kind(a.kind())));
                }
            }
            let mut event = ctx
                .event(EventKind::Call, node)
                .with("name", json!(name))
                .with("callee", json!(callee))
                .with(
                    "string_args",
                    Value::Array(string_args.into_iter().map(Value::String).collect()),
                )
                .with("arg_count", json!(positional.len()));
            // Only member calls have a receiver (the object before the method).
            if !receiver.is_empty() {
                event = event.with("receiver", json!(receiver));
            }
            // Per-position value + syntactic kind (capped to keep events small).
            for (i, (val, kind)) in positional.iter().take(32).enumerate() {
                event = event
                    .with(&format!("arg{i}"), json!(val))
                    .with(&format!("arg{i}_kind"), json!(kind));
            }
            Some(event)
        }

        "interface_declaration" => Some(type_decl(node, ctx, "interface")),
        "type_alias_declaration" => Some(type_decl(node, ctx, "type")),

        "function_declaration" | "function_signature" | "generator_function_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text(n, src).to_string())
                .unwrap_or_default();
            Some(ctx.event(EventKind::FunctionDecl, node).with("name", json!(name)))
        }

        "member_expression" => env_access(node, ctx),
        "subscript_expression" => env_subscript(node, ctx),

        "comment" => Some(
            ctx.event(EventKind::Comment, node)
                .with("text", json!(text(node, src))),
        ),

        "string" => Some(
            ctx.event(EventKind::StringLiteral, node)
                .with("value", json!(string_value(node, src))),
        ),

        _ => None,
    }
}

fn type_decl(node: Node, ctx: &Ctx, decl_kind: &str) -> Event {
    let name = node
        .child_by_field_name("name")
        .map(|n| text(n, ctx.src).to_string())
        .unwrap_or_default();
    ctx.event(EventKind::TypeDecl, node)
        .with("decl_kind", json!(decl_kind))
        .with("name", json!(name))
}

/// `process.env.X` — a member access whose object is `process.env`.
fn env_access(node: Node, ctx: &Ctx) -> Option<Event> {
    let obj = node.child_by_field_name("object")?;
    if text(obj, ctx.src) != "process.env" {
        return None;
    }
    let name = node
        .child_by_field_name("property")
        .map(|p| text(p, ctx.src).to_string())
        .unwrap_or_default();
    Some(
        ctx.event(EventKind::EnvAccess, node)
            .with("name", json!(name))
            .with("via", json!("process.env")),
    )
}

/// `process.env["X"]` — a subscript whose object is `process.env`.
fn env_subscript(node: Node, ctx: &Ctx) -> Option<Event> {
    let obj = node.child_by_field_name("object")?;
    if text(obj, ctx.src) != "process.env" {
        return None;
    }
    let name = node
        .child_by_field_name("index")
        .filter(|i| i.kind() == "string")
        .map(|i| string_value(i, ctx.src))
        .unwrap_or_else(|| "<dynamic>".to_string());
    Some(
        ctx.event(EventKind::EnvAccess, node)
            .with("name", json!(name))
            .with("via", json!("process.env")),
    )
}

fn collect_import_symbols(clause: Node, src: &[u8], out: &mut Vec<String>) {
    let mut cur = clause.walk();
    for ch in clause.named_children(&mut cur) {
        match ch.kind() {
            // default import: `import Foo from "..."`
            "identifier" => out.push(text(ch, src).to_string()),
            // namespace import: `import * as ns from "..."`
            "namespace_import" => {
                let mut c2 = ch.walk();
                for id in ch.named_children(&mut c2) {
                    if id.kind() == "identifier" {
                        out.push(text(id, src).to_string());
                    }
                }
            }
            // named imports: `import { a, b as c } from "..."`
            "named_imports" => {
                let mut c2 = ch.walk();
                for spec in ch.named_children(&mut c2) {
                    if spec.kind() == "import_specifier" {
                        let name = spec
                            .child_by_field_name("name")
                            .map(|n| text(n, src).to_string())
                            .unwrap_or_else(|| text(spec, src).to_string());
                        out.push(name);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Extracted, LanguageFrontend, SourceFile};

    fn extract(text: &str, want_tokens: bool) -> Extracted {
        TsJsFrontend
            .extract(
                &SourceFile {
                    path: Utf8PathBuf::from("x.ts"),
                    text,
                },
                want_tokens,
            )
            .unwrap()
    }

    #[test]
    fn token_stream_is_compact_and_canonical_overlay_is_separate() {
        let src = "switch (x) { default: foo(); }";

        // Tokens off: no token stream, but the canonical overlay is present.
        let off = extract(src, false);
        assert!(off.tokens.is_none());
        assert!(off.events.iter().any(|e| e.kind == EventKind::Call));

        // Tokens on: the compact stream has the named construct AND the bare
        // symbol, distinguished by `named`.
        let on = extract(src, true);
        let ts = on.tokens.as_ref().expect("token stream");
        let has = |nk: &str, named: bool| {
            ts.tokens
                .iter()
                .any(|t| ts.interner.resolve(t.kind) == nk && t.named == named)
        };
        assert!(has("switch_statement", true));
        assert!(has("switch", false));
        // ... and the canonical overlay is still produced from the same parse.
        assert!(on.events.iter().any(|e| e.kind == EventKind::Call));
    }

    #[test]
    fn jmp_links_matching_delimiters() {
        let on = extract("function f() {}", true);
        let ts = on.tokens.as_ref().unwrap();
        let open = ts
            .tokens
            .iter()
            .position(|t| ts.interner.resolve(t.kind) == "{")
            .expect("an opening brace token");
        let j = ts.tokens[open].jmp;
        assert_ne!(j, codepolicy_events::NO_JMP, "brace should link to its match");
        assert_eq!(ts.interner.resolve(ts.tokens[j as usize].kind), "}");
        assert_eq!(ts.tokens[j as usize].jmp as usize, open);
    }
}
