#!/bin/bash
# Release script: bump version, update changelog, tag, and optionally push.
# Usage: ./scripts/release.sh <major|minor|patch> [--push]
set -euo pipefail

PUSH=false
if [[ "${2:-}" == "--push" ]]; then
    PUSH=true
fi

BUMP_TYPE="${1:-}"
if [[ -z "$BUMP_TYPE" ]] || [[ ! "$BUMP_TYPE" =~ ^(major|minor|patch)$ ]]; then
    echo "Usage: $0 <major|minor|patch> [--push]"
    exit 1
fi

cd "$(git rev-parse --show-toplevel)"
if [[ -n "$(git status --porcelain)" ]]; then
    echo "Error: release preparation requires a clean working tree"
    exit 1
fi
command -v python3 >/dev/null
NEW_VERSION=$(python3 scripts/prepare-release.py "$BUMP_TYPE")
git check-ref-format "refs/tags/v${NEW_VERSION}"
if git rev-parse --verify "refs/tags/v${NEW_VERSION}" >/dev/null 2>&1; then
    echo "Error: release tag already exists: v${NEW_VERSION}"
    exit 1
fi
# Validate the exact prepared tree before creating a release commit or tag.
cargo check --locked --workspace --all-targets

# Generate changelog entry
CHANGELOG_ENTRY="## v${NEW_VERSION} ($(date +%Y-%m-%d))\n\n"
CHANGELOG_ENTRY+="### Changes\n\n"

# Collect commits since last tag
LAST_TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
if [[ -n "$LAST_TAG" ]]; then
    CHANGELOG_ENTRY+=$(git log "${LAST_TAG}..HEAD" --pretty=format:"- %s (%h)" --no-merges)
else
    CHANGELOG_ENTRY+=$(git log --pretty=format:"- %s (%h)" --no-merges -20)
fi
CHANGELOG_ENTRY+="\n\n"

# Prepend to CHANGELOG.md (create if missing)
if [[ -f CHANGELOG.md ]]; then
    echo -e "${CHANGELOG_ENTRY}" | cat - CHANGELOG.md > CHANGELOG.tmp && mv CHANGELOG.tmp CHANGELOG.md
else
    echo -e "# Changelog\n\n${CHANGELOG_ENTRY}" > CHANGELOG.md
fi
echo "Updated CHANGELOG.md"

# Commit and tag
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "release: v${NEW_VERSION}"
git tag -a "v${NEW_VERSION}" -m "Release v${NEW_VERSION}"

echo ""
echo "Created commit and tag v${NEW_VERSION}"

if [[ "$PUSH" == "true" ]]; then
    git push origin HEAD
    git push origin "v${NEW_VERSION}"
    echo "Pushed to origin."
else
    echo "Run 'git push origin HEAD && git push origin v${NEW_VERSION}' to publish."
fi
