#!/bin/sh
# Synthetic checks: no vendor executable or network access.
case "$1" in
  pass) printf 'check passed\n'; printf 'diagnostic stderr\n' >&2; exit 0 ;;
  fail) printf 'check failed\n'; exit 1 ;;
  json) printf 'runner preamble\n%s\n' "$2"; exit "$3" ;;
  text) printf '%s\n' "$2"; exit "$3" ;;
  literal) test "$2" = 'literal $(touch injected) ; * space' || exit 2; test "$HERM_TEST_ENV" = 'configured value' || exit 3; test "$PWD" = "$3" || exit 4 ;;
  noisy) i=0; while test "$i" -lt 3000; do printf 'long check output that belongs in an attempt log, not the git note\n'; i=$((i+1)); done; printf 'stderr trailer\n' >&2 ;;
  sleep) /bin/sleep 30 ;;
  *) exit 2 ;;
esac
