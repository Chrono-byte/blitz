#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$script_dir/../../../.." && pwd)
demo="$repo/crates/blitz-runtime/examples/irc-chat-v2"
build_target="$repo/target/irc-chat-v2-build"
built_blitz="$build_target/debug/blitz"
run_directory=$(mktemp -d "${TMPDIR:-/tmp}/blitz-irc-chat-v2.XXXXXX")
blitz="$run_directory/blitz"

if [[ $(uname -s) != Darwin ]] || ! command -v osascript >/dev/null; then
  echo "run-demo.sh requires macOS and Ghostty 1.3.0 or newer" >&2
  exit 1
fi

(cd -- "$repo" && cargo build -p blitz-cli --target-dir "$build_target")

if [[ ! -f "$built_blitz" ]]; then
  echo "Expected Blitz executable at $built_blitz" >&2
  exit 1
fi

cp -- "$built_blitz" "$blitz"

osascript - "$repo" "$blitz" "$demo/server.mjs" "$demo/alice.mjs" "$demo/bob.mjs" <<'APPLESCRIPT'
on run argv
  set repoPath to item 1 of argv
  set blitzPath to item 2 of argv
  set serverPath to item 3 of argv
  set alicePath to item 4 of argv
  set bobPath to item 5 of argv

  tell application "Ghostty"
    activate

    set serverConfig to new surface configuration
    set initial working directory of serverConfig to repoPath
    set command of serverConfig to quoted form of blitzPath & " run --module " & quoted form of serverPath

    set aliceConfig to new surface configuration
    set initial working directory of aliceConfig to repoPath
    set command of aliceConfig to quoted form of blitzPath & " " & quoted form of alicePath

    set bobConfig to new surface configuration
    set initial working directory of bobConfig to repoPath
    set command of bobConfig to quoted form of blitzPath & " " & quoted form of bobPath

    set chatWindow to new window with configuration serverConfig
    set serverPane to terminal 1 of selected tab of chatWindow

    repeat with attempt from 1 to 60
      try
        do shell script "/usr/bin/curl --fail --silent --max-time 1 http://127.0.0.1:8000/health >/dev/null"
        exit repeat
      on error
        if attempt is 60 then error "IRC chat v2 server did not become ready within 60 seconds"
        delay 1
      end try
    end repeat

    set alicePane to split serverPane direction right with configuration aliceConfig
    set bobPane to split alicePane direction down with configuration bobConfig
    focus alicePane
  end tell
end run
APPLESCRIPT
