//! Manifest frontend (proposal §7.2.1, §11.3).
//!
//! Emits a `PackageAdded` event for each dependency declared in a
//! `package.json`. Without `--diff` mode this means "present in the manifest"
//! rather than "newly added"; the name is kept for vocabulary consistency.

use camino::{Utf8Path, Utf8PathBuf};
use codepolicy_events::{Event, EventKind, Language, Span};
use serde_json::{json, Value};

use super::{Extracted, LanguageFrontend, SourceFile};

pub struct ManifestFrontend;

impl LanguageFrontend for ManifestFrontend {
    fn name(&self) -> &'static str {
        "manifest"
    }

    fn supports_file(&self, path: &Utf8Path) -> bool {
        path.file_name() == Some("package.json")
    }

    // A structured-only frontend: it produces canonical `PackageAdded` events
    // and no token stream (a manifest has no lexical source tokens).
    fn extract(&self, file: &SourceFile<'_>, _want_tokens: bool) -> anyhow::Result<Extracted> {
        let json: Value = serde_json::from_str(file.text)
            .map_err(|e| anyhow::anyhow!("invalid package.json {}: {e}", file.path))?;

        let mut events = Vec::new();
        for section in [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ] {
            let Some(Value::Object(deps)) = json.get(section) else {
                continue;
            };
            for (name, version) in deps {
                events.push(package_event(&file.path, section, name, version));
            }
        }
        Ok(Extracted {
            tokens: None,
            events,
        })
    }
}

fn package_event(file: &Utf8PathBuf, section: &str, name: &str, version: &Value) -> Event {
    // The manifest is not source code, so there is no meaningful span; use a
    // unit span at the top of the file.
    let span = Span {
        start_byte: 0,
        end_byte: 0,
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    };
    Event::new(EventKind::PackageAdded, Language::Javascript, file.clone(), span)
        .with("name", json!(name))
        .with("version", version.clone())
        .with("section", json!(section))
}
