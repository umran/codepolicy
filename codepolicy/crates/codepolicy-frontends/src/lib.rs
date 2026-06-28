//! Language frontends (proposal §6, §10.2).
//!
//! Each frontend turns a source file into a flat stream of canonical events.
//! v0 ships a Tree-sitter TypeScript/JavaScript frontend plus a `package.json`
//! manifest frontend that produces `PackageAdded` events.

use camino::{Utf8Path, Utf8PathBuf};
use codepolicy_events::{Event, TokenStream};

mod manifest;
mod ts_js;

pub use manifest::ManifestFrontend;
pub use ts_js::TsJsFrontend;

/// A source file handed to a frontend: its path plus a borrow of its text.
pub struct SourceFile<'a> {
    pub path: Utf8PathBuf,
    pub text: &'a str,
}

/// A frontend's output. The **token stream** is the universal, primary model
/// (every source frontend produces it, by whatever means — lexer or parser);
/// the **canonical event stream** is an optional, structured overlay a frontend
/// produces only if it can (e.g. by parsing).
#[derive(Default)]
pub struct Extracted {
    /// The compact, language-local token stream (Cobra-style). `Some` when
    /// `want_tokens`; the always-available primary layer.
    pub tokens: Option<TokenStream>,
    /// Optional normalized, cross-language events (Import, Call, EnvAccess, …).
    pub events: Vec<Event>,
}

/// A language frontend (proposal §10.2).
pub trait LanguageFrontend: Sync {
    /// Human-readable frontend name (for diagnostics).
    fn name(&self) -> &'static str;
    /// Whether this frontend handles the given path.
    fn supports_file(&self, path: &Utf8Path) -> bool;
    /// Extract from a file. When `want_tokens`, fill `Extracted.tokens` with the
    /// universal per-node token stream; `Extracted.events` is the optional
    /// structured overlay (a frontend that only lexes leaves it empty).
    fn extract(&self, file: &SourceFile<'_>, want_tokens: bool) -> anyhow::Result<Extracted>;
}

/// The default set of v0 frontends.
pub fn default_frontends() -> Vec<Box<dyn LanguageFrontend>> {
    vec![Box::new(TsJsFrontend), Box::new(ManifestFrontend)]
}

/// Find the first frontend that supports `path`.
pub fn frontend_for<'a>(
    frontends: &'a [Box<dyn LanguageFrontend>],
    path: &Utf8Path,
) -> Option<&'a dyn LanguageFrontend> {
    frontends
        .iter()
        .find(|f| f.supports_file(path))
        .map(|f| f.as_ref())
}
