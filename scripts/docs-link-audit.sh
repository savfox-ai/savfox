#!/bin/bash
# Audit documentation for broken internal links.
# Usage: ./scripts/docs-link-audit.sh
set -euo pipefail

DOCS_DIR="docs/en"
ERRORS=0

echo "Auditing documentation links in $DOCS_DIR..."
echo ""

# Find all markdown files
while IFS= read -r -d '' file; do
    # Extract markdown links: [text](path)
    while IFS= read -r link; do
        # Skip external URLs, anchors, and empty links
        if [[ "$link" =~ ^https?:// ]] || [[ "$link" =~ ^# ]] || [[ -z "$link" ]]; then
            continue
        fi

        # Strip anchor from link
        link_path="${link%%#*}"
        if [[ -z "$link_path" ]]; then
            continue
        fi

        # Resolve relative to the file's directory
        file_dir=$(dirname "$file")
        target="${file_dir}/${link_path}"

        if [[ ! -f "$target" ]]; then
            echo "BROKEN: $file -> $link (target: $target)"
            ERRORS=$((ERRORS + 1))
        fi
    done < <(grep -oP '\]\(\K[^)]+' "$file" 2>/dev/null || true)
done < <(find "$DOCS_DIR" -name '*.md' -print0)

echo ""
if [[ $ERRORS -eq 0 ]]; then
    echo "All links valid."
else
    echo "Found $ERRORS broken link(s)."
    exit 1
fi
