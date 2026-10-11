#!/usr/bin/env bash
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
  perl -0777 -i.bak -pe '
    s/\bRc::clone\s*\(\s*&\s*([A-Za-z0-9_:\.\->\(\)\[\] ]+?)\s*\)/$1.clone()/gs;
  ' "$f"
  if ! cmp -s "$f" "$f.bak"; then
    rm "$f.bak"
    echo "Updated: $f"
  else
    mv "$f.bak" "$f"
  fi
done

echo "Done."
