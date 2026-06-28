//! Canonical, language-neutral event model (proposal §7).
//!
//! Each language frontend emits a flat stream of [`Event`]s. Rules in the
//! matcher operate over these events, never over raw source text.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The languages a frontend can produce events for.
///
/// `#[serde(rename_all = "lowercase")]` keeps the JSON representation
/// (`"typescript"`, `"go"`, …) aligned with the proposal's examples (§7.3).
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
    /// Best-effort language detection from a file extension.
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

/// The canonical event vocabulary (proposal §7.2).
///
/// Uses serde's default representation, so the JSON `kind` value is the exact
/// PascalCase variant name (e.g. `EnvAccess` -> `"EnvAccess"`). Do **not** add
/// `#[serde(rename_all = ...)]` here, or it diverges from the documented
/// examples and from the YAML rule files (`event: Import`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EventKind {
    File,
    PackageAdded,
    Import,
    Export,
    Call,
    NewExpr,
    TypeDecl,
    FunctionDecl,
    MethodDecl,
    ClassDecl,
    Attribute,
    EnvAccess,
    StringLiteral,
    Comment,
    ScopeStart,
    ScopeEnd,
    GraphqlOperation,
    GeneratedFile,
    ConfigFile,
    /// Generic, language-local node (Cobra-style): carries `node_kind` (the
    /// frontend's native node type, e.g. `switch_statement`) and `text`. Lets a
    /// rule match *any* construct, not just the canonical kinds above — which
    /// are the normalized, cross-language views. Emitted only when a rule asks
    /// for it (see `references_kind`), to keep canonical-only runs cheap.
    Token,
}

/// A source span (proposal §7.1).
///
/// Conventions: lines and columns are **1-based**; `end_line`/`end_col`/
/// `end_byte` are **exclusive** (half-open). `start_byte`/`end_byte` are
/// 0-based byte offsets and are omitted from most JSON examples for brevity,
/// hence `#[serde(default)]`.
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

/// A single canonical event with its evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub kind: EventKind,
    pub language: Language,
    /// Shared across all events of a file (an `Arc`), so emitting one event per
    /// node — Cobra-style — doesn't reclone the path per token.
    pub file: Arc<Utf8PathBuf>,
    pub span: Span,
    #[serde(default)]
    pub attrs: BTreeMap<String, Value>,
}

impl Event {
    pub fn new(
        kind: EventKind,
        language: Language,
        file: impl Into<Arc<Utf8PathBuf>>,
        span: Span,
    ) -> Self {
        Event {
            kind,
            language,
            file: file.into(),
            span,
            attrs: BTreeMap::new(),
        }
    }

    /// Builder-style attribute insertion.
    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.attrs.insert(key.to_string(), value);
        self
    }

    /// Normalize an attribute to a flat list of string forms, the shape the
    /// matcher works with. Returns an empty vector if the attribute is absent.
    pub fn attr_strings(&self, key: &str) -> Vec<String> {
        self.attrs.get(key).map(value_strings).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Compact token stream (Cobra-style primary model)
// ---------------------------------------------------------------------------

/// An interned-string id within one [`TokenStream`].
pub type Sym = u32;

/// Sentinel for "no matching delimiter".
pub const NO_JMP: u32 = u32::MAX;

/// Per-stream string interner. Node kinds (`switch_statement`, `identifier`, …)
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

/// A compact lexical/CST token (Cobra-style). `Copy` and field-packed, so the
/// stream is one contiguous `Vec` with no per-token heap allocation; prev/next
/// are implicit (`i-1`/`i+1`) and `jmp` links matching delimiters.
#[derive(Debug, Clone, Copy)]
pub struct Token {
    /// Interned native node kind (e.g. `switch_statement`, `identifier`, `{`).
    pub kind: Sym,
    /// Interned source text (capped).
    pub text: Sym,
    /// Interned enclosing-function name (resolves to "" when none).
    pub func: Sym,
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

/// The token stream for one file — the universal, compact primary model.
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

    /// Resolve to readable JSON objects (for `codepolicy events --tokens`).
    pub fn resolved_json(&self) -> Vec<Value> {
        self.tokens
            .iter()
            .map(|t| {
                serde_json::json!({
                    "kind": "Token",
                    "node_kind": self.interner.resolve(t.kind),
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

/// Flatten a JSON value into the list of string forms used for matching:
/// a scalar becomes a single element, an array becomes its (recursively
/// flattened) elements, and structural/null values become nothing.
pub fn value_strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(value_strings).collect(),
        Value::Bool(b) => vec![b.to_string()],
        Value::Number(n) => vec![n.to_string()],
        Value::Null | Value::Object(_) => vec![],
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
    fn eventkind_uses_pascalcase() {
        assert_eq!(
            serde_json::to_string(&EventKind::EnvAccess).unwrap(),
            "\"EnvAccess\""
        );
    }

    #[test]
    fn span_deserializes_without_byte_fields() {
        // Mirrors the §7.3 examples, which omit start_byte/end_byte.
        let s: Span = serde_json::from_str(
            r#"{ "start_line": 1, "start_col": 1, "end_line": 1, "end_col": 43 }"#,
        )
        .unwrap();
        assert_eq!(s.start_byte, 0);
        assert_eq!(s.end_col, 43);
    }

    #[test]
    fn value_strings_flattens_scalars_and_arrays() {
        assert_eq!(value_strings(&serde_json::json!("a")), vec!["a"]);
        assert_eq!(
            value_strings(&serde_json::json!(["a", "b"])),
            vec!["a", "b"]
        );
        assert!(value_strings(&Value::Null).is_empty());
    }
}
