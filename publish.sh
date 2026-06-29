#!/usr/bin/env bash
#
# Publish the tyrant workspace to crates.io in dependency order.
#
# Crates must be published bottom-up: a crate can only be published after every
# crate it depends on is already on the registry. `cargo publish` waits for each
# upload to appear in the index before returning, so the next one resolves.
#
# Usage:
#   ./publish.sh             # real publish (requires `cargo login` first)
#   ./publish.sh --dry-run   # leaf crate verifies fully; dependents only validate
#                            #   packaging until their deps are actually published
#
# Any extra args are forwarded to each `cargo publish` invocation.
set -euo pipefail

cd "$(dirname "$0")/tyrant"

# Dependency order: token has no internal deps; the CLI (`tyrant`) depends on all.
CRATES=(
  tyrant-token
  tyrant-rules
  tyrant-frontends
  tyrant-match
  tyrant-report
  tyrant-core
  tyrant
)

for c in "${CRATES[@]}"; do
  echo ">>> cargo publish -p ${c} $*"
  cargo publish -p "$c" "$@"
done

echo "Done. All ${#CRATES[@]} crates published."
