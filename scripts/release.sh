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

# Find current version from root Cargo.toml
CURRENT_VERSION=$(grep '^version\s*=' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
if [[ -z "$CURRENT_VERSION" ]]; then
    echo "Error: Could not determine current version from Cargo.toml"
    exit 1
fi

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"

case "$BUMP_TYPE" in
    major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
    minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
    patch) PATCH=$((PATCH + 1)) ;;
esac

NEW_VERSION="${MAJOR}.${MINOR}.${PATCH}"
echo "Bumping version: $CURRENT_VERSION -> $NEW_VERSION"

# Update version in workspace Cargo.toml
sed -i "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" Cargo.toml
echo "Updated Cargo.toml"

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
git add Cargo.toml CHANGELOG.md
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
