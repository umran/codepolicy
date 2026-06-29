//! Orchestration: discover files, lex them with the frontends, load escape
//! hatches, and run the matcher.

use camino::{Utf8Path, Utf8PathBuf};
use codepolicy_token::TokenStream;
use codepolicy_frontends::{frontend_for, frontends, Frontend, SourceFile};
use codepolicy_match::{run, AdrRecord, MatchContext, Violation, WaiverRecord};
use codepolicy_rules::CompiledRule;
use rayon::prelude::*;

const DEFAULT_WAIVERS_DIR: &str = ".codepolicy/waivers";
const DEFAULT_ADR_DIR: &str = "docs/adr";

/// Everything needed to run a check against a repository root.
pub struct Project {
    pub root: Utf8PathBuf,
    pub frontends: Vec<Box<dyn Frontend>>,
}

impl Project {
    pub fn new(root: impl Into<Utf8PathBuf>) -> Self {
        Project {
            root: root.into(),
            frontends: frontends(),
        }
    }

    /// Discover candidate files under the root, honoring `.gitignore`.
    pub fn discover(&self) -> Vec<Utf8PathBuf> {
        let mut files = Vec::new();
        for result in ignore::WalkBuilder::new(&self.root).build() {
            let Ok(entry) = result else { continue };
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let Ok(path) = Utf8PathBuf::from_path_buf(entry.into_path()) else {
                continue; // skip non-UTF-8 paths
            };
            if frontend_for(&self.frontends, &path).is_some() {
                files.push(path);
            }
        }
        files.sort();
        files
    }

    /// Path relative to the project root (forward-slash), for glob matching
    /// and reporting.
    fn rel(&self, path: &Utf8Path) -> Utf8PathBuf {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_path_buf()
    }

    /// Lex a single file into its token stream. Paths are repo-relative.
    pub fn lex_file(&self, path: &Utf8Path) -> anyhow::Result<Option<TokenStream>> {
        let Some(frontend) = frontend_for(&self.frontends, path) else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {path}: {e}"))?;
        let rel = self.rel(path);
        let source = SourceFile {
            path: rel,
            text: &text,
        };
        Ok(Some(frontend.lex(&source)?))
    }

    /// Lex all discovered files, in parallel, into their token streams.
    pub fn lex_all(&self) -> Vec<TokenStream> {
        self.discover()
            .par_iter()
            .filter_map(|path| {
                self.lex_file(path).unwrap_or_else(|err| {
                    eprintln!("warning: {err}");
                    None
                })
            })
            .collect()
    }

    /// Load waivers and ADRs from the configured (or default) directories.
    pub fn load_context(
        &self,
        waivers_dir: Option<&str>,
        adr_dir: Option<&str>,
    ) -> MatchContext {
        let waivers_dir = waivers_dir.unwrap_or(DEFAULT_WAIVERS_DIR);
        let adr_dir = adr_dir.unwrap_or(DEFAULT_ADR_DIR);
        MatchContext {
            waivers: load_waivers(&self.root.join(waivers_dir)),
            adrs: load_adrs(&self.root.join(adr_dir)),
        }
    }

    /// Full pipeline: lex all files, then match.
    pub fn check(&self, rules: &[CompiledRule], ctx: &MatchContext) -> Vec<Violation> {
        let tokens = self.lex_all();
        run(rules, &tokens, ctx)
    }
}

fn yaml_files(dir: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        if let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) {
            if matches!(path.extension(), Some("yaml" | "yml")) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn load_waivers(dir: &Utf8Path) -> Vec<WaiverRecord> {
    let mut out = Vec::new();
    for path in yaml_files(dir) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) else {
            eprintln!("warning: could not parse waiver {path}");
            continue;
        };
        if let (Some(rule), Some(file)) = (
            value.get("rule").and_then(|v| v.as_str()),
            value.get("file").and_then(|v| v.as_str()),
        ) {
            out.push(WaiverRecord {
                rule: rule.to_string(),
                file: file.to_string(),
            });
        }
    }
    out
}

fn load_adrs(dir: &Utf8Path) -> Vec<AdrRecord> {
    let mut out = Vec::new();
    for path in yaml_files(dir) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) else {
            eprintln!("warning: could not parse ADR {path}");
            continue;
        };
        if let Some(topic) = value.get("topic").and_then(|v| v.as_str()) {
            let accepted = value
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case("accepted"))
                .unwrap_or(false);
            out.push(AdrRecord {
                topic: topic.to_string(),
                accepted,
            });
        }
    }
    out
}
