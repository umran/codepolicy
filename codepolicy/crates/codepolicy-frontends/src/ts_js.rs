//! TypeScript / JavaScript frontend built on Tree-sitter.
//!
//! Lexes a file into a flat stream of leaf lexemes. A string or template literal
//! is one atomic token (matching never reaches inside it); comments are tokens
//! too (class `comment`); composite parse nodes are not tokens. Each lexeme is
//! tagged with a normalized `class` so `@class` matchers stay language-agnostic.

use camino::{Utf8Path, Utf8PathBuf};
use codepolicy_token::{Interner, Language, Token, TokenStream, NO_JMP};
use std::sync::Arc;
use tree_sitter::{Language as TsLanguage, Node, Parser};

use super::{Frontend, SourceFile};

pub struct TsJsFrontend;

const NAME: &str = "ts_js";

fn supports(path: &Utf8Path) -> bool {
    matches!(
        path.extension(),
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
    )
}

impl Frontend for TsJsFrontend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn supports_file(&self, path: &Utf8Path) -> bool {
        supports(path)
    }

    fn lex(&self, file: &SourceFile<'_>) -> anyhow::Result<TokenStream> {
        let ext = file.path.extension().unwrap_or("");
        let (language, ts_lang) = select_language(ext);
        let mut parser = Parser::new();
        parser
            .set_language(&ts_lang)
            .map_err(|e| anyhow::anyhow!("failed to load grammar: {e}"))?;
        let tree = parser
            .parse(file.text.as_bytes(), None)
            .ok_or_else(|| anyhow::anyhow!("parser returned no tree for {}", file.path))?;

        let mut tb = TokenBuilder::default();
        walk_tokens(tree.root_node(), file.text.as_bytes(), &mut tb, &Frame::default());
        tb.finish_jmp();
        let file_arc: Arc<Utf8PathBuf> = Arc::new(file.path.clone());
        Ok(TokenStream {
            file: file_arc,
            language,
            interner: tb.interner,
            tokens: tb.tokens,
        })
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

fn text<'a>(n: Node, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

/// Truncate to at most `max` characters (a string/comment lexeme's text can be
/// long).
fn capped(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// The normalized, language-neutral **lexeme class** a `@class` matcher tests.
/// Maps this grammar's leaf node kinds to a small shared vocabulary; another
/// language's frontend maps its own kinds to the same class names, so a rule
/// like `@ident` is language-agnostic.
fn classify(node: Node) -> &'static str {
    match node.kind() {
        "identifier" => "ident",
        "property_identifier" | "shorthand_property_identifier" => "prop",
        "number" => "num",
        "string" | "template_string" => "str",
        "true" | "false" => "bool",
        "regex" => "regex",
        "null" => "null",
        "undefined" => "undefined",
        "comment" => "comment",
        // Anonymous symbols (keywords, operators, punctuation) carry their text
        // as the node kind — match those by text. Any other named leaf is `other`.
        _ if node.is_named() => "other",
        _ => "symbol",
    }
}

/// Nesting depths and the enclosing function, accumulated as the walk descends.
/// Mirrors Cobra's `.curly`/`.round`/`.bracket`/`.fct` fields.
#[derive(Clone, Default)]
struct Frame {
    curly: u32,
    round: u32,
    bracket: u32,
    function: Option<String>,
}

/// Emit the **lexeme stream**: one token per leaf lexeme, in source order. A
/// string or template literal is a single atomic token (its interior is never a
/// separate token, so matching never reaches inside it). Comments are tokens too
/// (class `comment`). Composite nodes are not tokens; only their leaves are.
fn walk_tokens(node: Node, src: &[u8], tb: &mut TokenBuilder, frame: &Frame) {
    if matches!(node.kind(), "string" | "template_string") {
        tb.push(node, src, frame);
        return;
    }
    if node.child_count() == 0 {
        tb.push(node, src, frame);
        return;
    }
    let child = child_frame(node, src, frame);
    let mut cur = node.walk();
    for c in node.children(&mut cur) {
        walk_tokens(c, src, tb, &child);
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
        let class = self.interner.intern(classify(node));
        let s = node.start_position();
        let e = node.end_position();
        self.tokens.push(Token {
            kind,
            text,
            func,
            class,
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

/// Compute the frame for a node's children: increment the relevant nesting depth
/// and update the enclosing-function name.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Frontend, SourceFile};

    fn source(text: &str) -> SourceFile<'_> {
        SourceFile {
            path: Utf8PathBuf::from("x.ts"),
            text,
        }
    }

    #[test]
    fn lexes_a_leaf_stream() {
        let src = "switch (x) { foo; \"a string\"; } // a comment";
        let ts = TsJsFrontend.lex(&source(src)).unwrap();
        let kinds: Vec<&str> = ts.tokens.iter().map(|t| ts.interner.resolve(t.kind)).collect();
        // Leaf lexemes: the `switch` keyword, the `foo` identifier, the whole
        // string literal as ONE token, and the comment.
        assert!(kinds.contains(&"switch"));
        assert!(kinds.contains(&"identifier"));
        assert!(kinds.contains(&"string"));
        assert!(kinds.contains(&"comment"));
        // No composite parse nodes; no string interior as a separate token.
        assert!(!kinds.contains(&"switch_statement"));
        assert!(!kinds.contains(&"statement_block"));
        assert!(!kinds.contains(&"string_fragment"));
    }

    #[test]
    fn assigns_normalized_classes() {
        let ts = TsJsFrontend.lex(&source("const x = 1; // hi")).unwrap();
        let class_of = |nk: &str| {
            ts.tokens
                .iter()
                .find(|t| ts.interner.resolve(t.kind) == nk)
                .map(|t| ts.interner.resolve(t.class).to_string())
        };
        assert_eq!(class_of("identifier").as_deref(), Some("ident"));
        assert_eq!(class_of("number").as_deref(), Some("num"));
        assert_eq!(class_of("comment").as_deref(), Some("comment"));
        assert_eq!(class_of("const").as_deref(), Some("symbol"));
    }

    #[test]
    fn jmp_links_matching_delimiters() {
        let ts = TsJsFrontend.lex(&source("function f() {}")).unwrap();
        let open = ts
            .tokens
            .iter()
            .position(|t| ts.interner.resolve(t.kind) == "{")
            .expect("an opening brace token");
        let j = ts.tokens[open].jmp;
        assert_ne!(j, codepolicy_token::NO_JMP, "brace should link to its match");
        assert_eq!(ts.interner.resolve(ts.tokens[j as usize].kind), "}");
        assert_eq!(ts.tokens[j as usize].jmp as usize, open);
    }
}
