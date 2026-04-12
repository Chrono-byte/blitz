#!/usr/bin/env bash
# Replace Rc::clone(&x) with x.clone() across Rust source files under src/ and bin/
set -euo pipefail

echo "Normalizing Rc::clone(&x) -> x.clone() in .rs files..."

files=$(rg -l "Rc::clone\s*\(\s*&" src || true)
if [[ -z "$files" ]]; then
  echo "No matches found."
  exit 0
fi

echo "Files to update:"
echo "$files"

for f in $files; do
  # Use perl to do a conservative replace: Rc::clone(& SOME_EXPR ) -> SOME_EXPR.clone()
  # Capture a reasonably complex expr: allow :: and . and alphanum and _ and -> and parentheses
  perl -0777 -i.bak -pe '
    s/\bRc::clone\s*\(\s*&\s*([A-Za-z0-9_:\.\->\(\)\[\] ]+?)\s*\)/$1.clone()/gs;
  ' "$f"
  # If file changed, remove backup; otherwise restore
  if ! cmp -s "$f" "$f.bak"; then
    rm "$f.bak"
    echo "Updated: $f"
  else
    mv "$f.bak" "$f"
  fi
done

echo "Done."
