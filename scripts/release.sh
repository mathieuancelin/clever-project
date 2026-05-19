#!/usr/bin/env bash
#
# scripts/release.sh — cut a new release.
#
# Usage:
#   ./scripts/release.sh <version>
#
# Example:
#   ./scripts/release.sh 0.2.0
#
# What it does:
#   1. Validates the version looks like semver
#   2. Ensures we're on `main` with a clean tree, up-to-date with origin
#   3. Ensures the tag doesn't already exist (locally or on origin)
#   4. Bumps the `[package].version` in Cargo.toml
#   5. Runs fmt --check, clippy -D warnings, and the test suite (CI parity)
#   6. Refreshes Cargo.lock
#   7. Commits the bump, creates an annotated tag `vX.Y.Z`, pushes both
#      (triggers .github/workflows/release.yml on the remote)
#
set -euo pipefail

usage() {
  cat >&2 <<EOF
Usage: $0 <version>

  <version>   semver string, no leading "v" (e.g. 0.2.0, 1.0.0-rc.1)

Run from the project root.
EOF
  exit 2
}

[ $# -eq 1 ] || usage
VERSION="$1"
TAG="v$VERSION"

# --- preflight ---------------------------------------------------------------

if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.+][0-9A-Za-z.-]+)?$'; then
  echo "error: '$VERSION' does not look like a valid semver string (expected X.Y.Z)" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

if [ ! -f Cargo.toml ]; then
  echo "error: Cargo.toml not found in $ROOT" >&2
  exit 1
fi

BRANCH="$(git symbolic-ref --short HEAD)"
if [ "$BRANCH" != "main" ]; then
  echo "error: not on main (current branch: $BRANCH)" >&2
  exit 1
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "error: working tree has uncommitted changes; commit or stash first" >&2
  exit 1
fi

echo "==> fetching origin"
git fetch --tags origin main

if [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]; then
  echo "error: local main is not in sync with origin/main; pull or push before releasing" >&2
  exit 1
fi

if git rev-parse --verify --quiet "refs/tags/$TAG" >/dev/null; then
  echo "error: tag $TAG already exists locally" >&2
  exit 1
fi
if git ls-remote --tags origin "refs/tags/$TAG" | grep -q "refs/tags/$TAG$"; then
  echo "error: tag $TAG already exists on origin" >&2
  exit 1
fi

CURRENT_VERSION="$(awk -F'"' '/^version = "/ { print $2; exit }' Cargo.toml)"
if [ "$CURRENT_VERSION" = "$VERSION" ]; then
  echo "error: Cargo.toml already at version $VERSION (nothing to bump)" >&2
  exit 1
fi

echo "==> $CURRENT_VERSION  ->  $VERSION"

# --- bump Cargo.toml (first `version = "..."` line, i.e. the [package] one) --

awk -v v="$VERSION" '
  BEGIN { done = 0 }
  /^version = "/ && !done { sub(/"[^"]*"/, "\"" v "\""); done = 1 }
  { print }
' Cargo.toml > Cargo.toml.new
mv Cargo.toml.new Cargo.toml

git --no-pager diff -- Cargo.toml
echo

read -r -p "Looks good? Continue with fmt + clippy + tests + tag (y/N) " ans
case "$ans" in
  y|Y|yes|YES) ;;
  *)
    echo "aborting; reverting Cargo.toml"
    git checkout -- Cargo.toml
    exit 1
    ;;
esac

# --- local CI parity ---------------------------------------------------------

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --all-targets -- -D warnings

echo "==> cargo test"
cargo test --all

echo "==> refresh Cargo.lock"
cargo check --quiet

# --- commit + tag + push -----------------------------------------------------

git add Cargo.toml Cargo.lock
git commit -m "release: $TAG"
git tag -a "$TAG" -m "$TAG"

echo
echo "Ready to push:"
echo "  - commit $(git rev-parse --short HEAD) on main"
echo "  - tag $TAG"
read -r -p "Push to origin now? (y/N) " ans2
case "$ans2" in
  y|Y|yes|YES) ;;
  *)
    echo "skipping push. To finish manually:"
    echo "  git push origin main"
    echo "  git push origin $TAG"
    exit 0
    ;;
esac

git push origin main
git push origin "$TAG"

REMOTE_URL="$(git config remote.origin.url)"
SLUG="$(printf '%s' "$REMOTE_URL" | sed -E 's|.*github.com[:/](.+)\.git$|\1|; s|.*github.com[:/](.+)$|\1|')"

echo
echo "Pushed $TAG. The release workflow should be picking it up here:"
echo "  https://github.com/$SLUG/actions"
