//! Language frontends: lex a source file into its token (lexeme) stream.
//!
//! Ships a Tree-sitter TypeScript/JavaScript frontend. Adding a language is a
//! single trait implementation.

use camino::{Utf8Path, Utf8PathBuf};
use codepolicy_token::TokenStream;

mod ts_js;

pub use ts_js::TsJsFrontend;

/// A source file handed to a frontend: its path plus a borrow of its text.
pub struct SourceFile<'a> {
    pub path: Utf8PathBuf,
    pub text: &'a str,
}

/// Lexes a file into its compact token stream.
pub trait Frontend: Sync {
    /// Human-readable name (for diagnostics).
    fn name(&self) -> &'static str;
    /// Whether this frontend handles the given path.
    fn supports_file(&self, path: &Utf8Path) -> bool;
    /// Lex the file into its token stream.
    fn lex(&self, file: &SourceFile<'_>) -> anyhow::Result<TokenStream>;
}

/// The frontends shipped by default.
pub fn frontends() -> Vec<Box<dyn Frontend>> {
    vec![Box::new(TsJsFrontend)]
}

/// The first frontend that supports `path`.
pub fn frontend_for<'a>(
    frontends: &'a [Box<dyn Frontend>],
    path: &Utf8Path,
) -> Option<&'a dyn Frontend> {
    frontends
        .iter()
        .find(|f| f.supports_file(path))
        .map(|f| f.as_ref())
}
