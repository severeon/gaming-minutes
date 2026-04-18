#!/usr/bin/env bash
set -euo pipefail

bundle_path="${1:-minutes.mcpb}"

if [[ ! -f "$bundle_path" ]]; then
  echo "Missing MCPB bundle: $bundle_path" >&2
  exit 1
fi

python3 - <<'PY' "$bundle_path"
import sys
import zipfile

bundle = sys.argv[1]
required = [
    "crates/mcp/dist/index.js",
    "crates/mcp/node_modules/yaml/dist/nodes/addPairToJSMap.js",
    "crates/mcp/node_modules/yaml/dist/schema/yaml-1.1/merge.js",
    "crates/mcp/node_modules/yaml/dist/schema/yaml-1.1/schema.js",
]

with zipfile.ZipFile(bundle) as zf:
    names = set(zf.namelist())
    missing = [path for path in required if path not in names]
    # Claude Desktop 1.3109.0 rejects any zip entry containing `..` as path
    # traversal — even when the `..` is literal chars inside a filename.
    # Next.js chunk filenames do this routinely, so a stray `.vercel/output/`
    # or `.next/` tree at repo root sinks the whole bundle (issue #149).
    path_traversal = sorted(n for n in names if ".." in n)

if missing:
    print("MCPB bundle is missing required runtime files:", file=sys.stderr)
    for path in missing:
        print(f"  - {path}", file=sys.stderr)
    raise SystemExit(1)

if path_traversal:
    print(
        "MCPB bundle contains paths with '..' that Claude Desktop will reject "
        "as path traversal:",
        file=sys.stderr,
    )
    for path in path_traversal[:10]:
        print(f"  - {path}", file=sys.stderr)
    if len(path_traversal) > 10:
        print(f"  ... and {len(path_traversal) - 10} more", file=sys.stderr)
    print(
        "Usually caused by a stray .vercel/output/ or .next/ tree at repo "
        "root. Add those paths to .mcpbignore and repack.",
        file=sys.stderr,
    )
    raise SystemExit(1)

print(f"MCPB bundle looks healthy: {bundle}")
PY
