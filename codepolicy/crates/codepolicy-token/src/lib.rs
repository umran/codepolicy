//! The lexeme model: the lexer's output as a flat, compact token stream.
//!
//! Rules match over this stream of lexemes — keywords, operators, identifiers,
//! literals, punctuation, comments — never over raw source text.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// The languages a frontend can lex.
///
/// `#[serde(rename_all = "lowercase")]` keeps the JSON representation
/// (`"typescript"`, `"go"`, …) aligned with how `lang` is written in rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Typescript,
    Javascript,
    Go,
    Python,
    Rust,
}

impl Language {
    /// Best-effort language detection from a file extension (no leading dot).
    pub fn from_extension(ext: &str) -> Option<Language> {
        match ext {
            "ts" | "tsx" | "mts" | "cts" => Some(Language::Typescript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::Javascript),
            "go" => Some(Language::Go),
            "py" => Some(Language::Python),
            "rs" => Some(Language::Rust),
            _ => None,
        }
    }
}

/// A source span.
///
/// Conventions: lines and columns are **1-based**; `end_line`/`end_col`/
/// `end_byte` are **exclusive** (half-open). `start_byte`/`end_byte` are 0-based
/// byte offsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    #[serde(default)]
    pub start_byte: usize,
    #[serde(default)]
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

// ---------------------------------------------------------------------------
// Compact token stream (Cobra-style)
// ---------------------------------------------------------------------------

/// An interned-string id within one [`TokenStream`].
pub type Sym = u32;

/// Sentinel for "no matching delimiter".
pub const NO_JMP: u32 = u32::MAX;

/// Per-stream string interner. Node kinds (`identifier`, `switch`, …), classes,
/// and token texts repeat heavily, so interning makes each [`Token`] hold small
/// integer ids instead of owned strings.
#[derive(Debug, Default, Clone)]
pub struct Interner {
    strings: Vec<String>,
    map: HashMap<String, Sym>,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.strings.len() as Sym;
        self.strings.push(s.to_string());
        self.map.insert(s.to_string(), id);
        id
    }

    pub fn resolve(&self, id: Sym) -> &str {
        self.strings.get(id as usize).map(String::as_str).unwrap_or("")
    }
}

/// A compact lexeme. `Copy` and field-packed, so the stream is one contiguous
/// `Vec` with no per-token heap allocation; prev/next are implicit (`i-1`/`i+1`)
/// and `jmp` links matching delimiters.
#[derive(Debug, Clone, Copy)]
pub struct Token {
    /// Interned native node kind (e.g. `identifier`, `switch`, `==`, `{`).
    pub kind: Sym,
    /// Interned source text (capped).
    pub text: Sym,
    /// Interned enclosing-function name (resolves to "" when none).
    pub func: Sym,
    /// Interned normalized lexeme class assigned by the frontend — a small,
    /// language-neutral category (`ident`, `str`, `num`, `comment`, `symbol`, …)
    /// that `@class` matchers test, so rules stay language-agnostic.
    pub class: Sym,
    /// Whether this is a named node vs. a bare symbol literal.
    pub named: bool,
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub curly: u16,
    pub round: u16,
    pub bracket: u16,
    /// Index of the matching delimiter, or [`NO_JMP`].
    pub jmp: u32,
}

/// The token stream for one file.
#[derive(Debug, Clone)]
pub struct TokenStream {
    pub file: Arc<Utf8PathBuf>,
    pub language: Language,
    pub interner: Interner,
    pub tokens: Vec<Token>,
}

impl TokenStream {
    pub fn span_of(&self, t: &Token) -> Span {
        Span {
            start_byte: t.start_byte as usize,
            end_byte: t.end_byte as usize,
            start_line: t.start_line as usize,
            start_col: t.start_col as usize,
            end_line: t.end_line as usize,
            end_col: t.end_col as usize,
        }
    }

    /// Resolve to readable JSON objects (for `codepolicy tokens`).
    pub fn resolved_json(&self) -> Vec<Value> {
        self.tokens
            .iter()
            .map(|t| {
                serde_json::json!({
                    "node_kind": self.interner.resolve(t.kind),
                    "class": self.interner.resolve(t.class),
                    "text": self.interner.resolve(t.text),
                    "named": t.named,
                    "function": self.interner.resolve(t.func),
                    "curly_depth": t.curly,
                    "round_depth": t.round,
                    "bracket_depth": t.bracket,
                    "span": {
                        "start_byte": t.start_byte, "end_byte": t.end_byte,
                        "start_line": t.start_line, "start_col": t.start_col,
                        "end_line": t.end_line, "end_col": t.end_col,
                    },
                    "jmp": if t.jmp == NO_JMP { Value::Null } else { serde_json::json!(t.jmp) },
                })
            })
            .collect()
    }
}

/// A borrowed view of one [`Token`] paired with its stream's [`Interner`] — the
/// element the matcher works over. `field_strings` resolves a field name to the
/// flat list of strings predicates evaluate against.
#[derive(Debug, Clone, Copy)]
pub struct TokenRef<'a> {
    pub token: &'a Token,
    pub interner: &'a Interner,
}

impl TokenRef<'_> {
    pub fn field_strings(&self, key: &str) -> Vec<String> {
        let t = self.token;
        let s = match key {
            "node_kind" => self.interner.resolve(t.kind).to_string(),
            "class" => self.interner.resolve(t.class).to_string(),
            "text" => self.interner.resolve(t.text).to_string(),
            "function" => self.interner.resolve(t.func).to_string(),
            "named" => if t.named { "true" } else { "false" }.to_string(),
            "curly_depth" => t.curly.to_string(),
            "round_depth" => t.round.to_string(),
            "bracket_depth" => t.bracket.to_string(),
            "range_lines" => (t.end_line - t.start_line + 1).to_string(),
            "text_len" => (t.end_byte - t.start_byte).to_string(),
            _ => return Vec::new(),
        };
        vec![s]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_roundtrips_as_lowercase() {
        let j = serde_json::to_string(&Language::Typescript).unwrap();
        assert_eq!(j, "\"typescript\"");
        let l: Language = serde_json::from_str("\"go\"").unwrap();
        assert_eq!(l, Language::Go);
    }

    #[test]
    fn span_deserializes_without_byte_fields() {
        let s: Span = serde_json::from_str(
            r#"{ "start_line": 1, "start_col": 1, "end_line": 1, "end_col": 43 }"#,
        )
        .unwrap();
        assert_eq!(s.start_byte, 0);
        assert_eq!(s.end_col, 43);
    }
}
