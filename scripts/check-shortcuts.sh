#!/usr/bin/env sh
#
# check-shortcuts.sh — block symptom-hiding shortcuts from entering the tree.
#
# Scans added lines of Rust changes for the mechanical shortcut shapes that
# CLAUDE.md forbids (timers/sleeps, unwrap/expect/panic on expected conditions,
# todo!/unimplemented!, #[allow]/#[ignore], swallowed errors). It cannot catch
# conceptual shortcuts; those are the job of per-step review and the tests.
#
# Usage:
#   check-shortcuts.sh --staged         scan the staged diff (git pre-commit hook)
#   check-shortcuts.sh --worktree       scan working tree vs HEAD + untracked .rs
#   check-shortcuts.sh --range <range>  scan a git diff range (CI on a push/PR)
#
# The --range mode takes any expression `git diff` accepts, e.g. `BASE...HEAD`, so
# CI can scan exactly the lines a push or pull request introduces. A clean CI
# checkout has nothing staged or in the worktree, so --staged/--worktree would scan
# nothing there; --range is the CI entry point.
#
# Escape hatch: a deliberate, justified use is allowed by appending
#   // shortcut-ok: <reason>
# to the same line. This should be rare and is visible in review.
#
# Exit status: 0 = clean (warnings allowed), 1 = a blocking shortcut was found,
# 2 = bad invocation.

set -eu

mode="${1:---staged}"
range=""
case "$mode" in
  --staged | --worktree) ;;
  --range)
    range="${2:-}"
    if [ -z "$range" ]; then
      echo "usage: $0 --range <git-range>" >&2
      exit 2
    fi
    ;;
  *)
    echo "usage: $0 --staged|--worktree|--range <git-range>" >&2
    exit 2
    ;;
esac

# List the Rust files to scan, one per line, depending on mode. Vendored third-party crates
# under vendor/ are excluded: they are not ours to hold to the no-shortcuts rule. Newline-
# separated (not -z) so the reader below stays POSIX: the CI runner's /bin/sh is dash, whose
# `read` has no `-d`. Rust source paths never contain a newline, so this is safe.
changed_files() {
  if [ "$mode" = "--staged" ]; then
    git diff --cached --name-only --diff-filter=ACM -- '*.rs' ':!vendor/'
  elif [ "$mode" = "--range" ]; then
    git diff "$range" --name-only --diff-filter=ACM -- '*.rs' ':!vendor/'
  else
    git diff HEAD --name-only --diff-filter=ACM -- '*.rs' ':!vendor/'
    git ls-files --others --exclude-standard -- '*.rs' ':!vendor/'
  fi
}

# Emit the unified=0 diff (added lines + hunk headers) for one file.
file_diff() {
  f="$1"
  if [ "$mode" = "--staged" ]; then
    git diff --cached --unified=0 --no-color -- "$f"
  elif [ "$mode" = "--range" ]; then
    git diff "$range" --unified=0 --no-color -- "$f"
  elif git ls-files --error-unmatch -- "$f" >/dev/null 2>&1; then
    git diff HEAD --unified=0 --no-color -- "$f"
  else
    # Untracked file: treat the whole thing as added.
    git diff --no-index --unified=0 --no-color -- /dev/null "$f" || true
  fi
}

# First line of the file's test module, or 0 if none. The widely used
# convention is a `#[cfg(test)]` block at the bottom of the file; lines at or
# after it are exempt from the unwrap/expect/panic checks (idiomatic in tests).
cfg_test_start() {
  f="$1"
  if [ "$mode" = "--staged" ]; then
    git show ":$f" 2>/dev/null | grep -n -m1 -E '^[[:space:]]*#\[cfg\(test\)\]' | cut -d: -f1 || true
  else
    grep -n -m1 -E '^[[:space:]]*#\[cfg\(test\)\]' "$f" 2>/dev/null | cut -d: -f1 || true
  fi
}

status=0
tmp_files=$(mktemp)
changed_files >"$tmp_files"

# Iterate the paths without a subshell, so $status survives. Redirecting the temp file into the
# loop (rather than a pipe) is what keeps the loop in the current shell. The `|| [ -n "$f" ]`
# handles a final line with no trailing newline.
while IFS= read -r f || [ -n "$f" ]; do
  [ -n "$f" ] || continue

  # Whole-file test code (integration tests, benches) is exempt from the
  # unwrap/expect/panic tier entirely.
  case "$f" in
    tests/* | */tests/* | benches/* | */benches/*) test_all=1 ;;
    *) test_all=0 ;;
  esac

  cfg_start=$(cfg_test_start "$f")
  [ -n "${cfg_start:-}" ] || cfg_start=0

  out=$(
    file_diff "$f" | awk -v file="$f" -v cfgstart="$cfg_start" -v testall="$test_all" '
      function report(sev, msg) { printf "%s:%d: %s: %s\n", file, lineno, sev, msg }
      function in_test() { return testall == 1 || (cfgstart > 0 && lineno >= cfgstart) }

      /^@@/ {
        # @@ -a,b +c,d @@  -> new-file hunk starts at c
        if (match($0, /\+[0-9]+/)) curline = substr($0, RSTART + 1, RLENGTH - 1) + 0
        next
      }
      /^\+\+\+/ { next }
      /^\+/ {
        line = substr($0, 2)
        lineno = curline
        curline++

        if (index(line, "shortcut-ok") > 0) next

        # --- blocking tier: rarely legitimate anywhere ---
        if (line ~ /(^|[^A-Za-z0-9_])todo[[:space:]]*!/)          { report("ERROR", "todo! left in committed code"); err = 1 }
        if (line ~ /(^|[^A-Za-z0-9_])unimplemented[[:space:]]*!/) { report("ERROR", "unimplemented! left in committed code"); err = 1 }
        if (line ~ /(^|[^A-Za-z0-9_])sleep[[:space:]]*\(/)        { report("ERROR", "sleep/timer used to mask timing; fix the cause"); err = 1 }
        if (line ~ /#\[[[:space:]]*ignore/)                       { report("ERROR", "ignored test; fix or delete it, do not skip"); err = 1 }
        if (line ~ /#\[[[:space:]]*allow[[:space:]]*\(/)          { report("ERROR", "#[allow] silences a lint; fix the underlying code"); err = 1 }

        # --- blocking tier, but idiomatic in test code (exempted there) ---
        if (!in_test()) {
          if (line ~ /\.unwrap[[:space:]]*\(/)               { report("ERROR", "unwrap on an expected condition; handle the error"); err = 1 }
          if (line ~ /\.expect[[:space:]]*\(/)               { report("ERROR", "expect on an expected condition; handle the error"); err = 1 }
          if (line ~ /(^|[^A-Za-z0-9_])panic[[:space:]]*!/)  { report("ERROR", "panic! in library code; return an error instead"); err = 1 }
        }

        # --- warning tier: often fine, sometimes a swallowed error ---
        if (line ~ /(^|[^A-Za-z0-9_])let[[:space:]]+_[[:space:]]*=/) { report("warning", "discarded value; make sure no error is being swallowed") }
        if (line ~ /\.ok[[:space:]]*\([[:space:]]*\)[[:space:]]*;/)  { report("warning", "Result discarded via .ok(); make sure no error is being swallowed") }
      }
      END { exit err }
    '
  ) && file_status=0 || file_status=$?

  if [ -n "$out" ]; then
    printf '%s\n' "$out" >&2
  fi
  if [ "$file_status" -ne 0 ]; then
    status=1
  fi
done <"$tmp_files"

rm -f "$tmp_files"

if [ "$status" -ne 0 ]; then
  echo "" >&2
  echo "check-shortcuts: blocking shortcut(s) found. Fix the cause, or if a use is" >&2
  echo "genuinely justified, annotate that line with '// shortcut-ok: <reason>'." >&2
fi

exit "$status"
