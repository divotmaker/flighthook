#!/bin/sh
# Installs an opt-in git pre-push hook into this clone that runs
# `make screens-check` when the commits being pushed touch the UI, the app,
# the shared types or the screenshot scenes, and stops the push if a README
# screenshot no longer matches (see docs/SCREENSHOTS.md).
#
#   scripts/screens/install-hook.sh            install
#   scripts/screens/install-hook.sh --remove   remove
#
# A full check takes about 40 s with a warm build, so it runs before a push,
# not before every commit. `git push --no-verify` skips it once.
set -eu

hooks=$(git rev-parse --git-path hooks)
hook="$hooks/pre-push"
marker='# flighthook screens-check hook'

if [ "${1:-}" = "--remove" ]; then
    if [ -f "$hook" ] && grep -q "$marker" "$hook"; then
        rm "$hook"
        echo "removed $hook"
    fi
    exit 0
fi

if [ -e "$hook" ] && ! grep -q "$marker" "$hook"; then
    echo "$hook exists and is not this hook; not replacing it" >&2
    exit 1
fi

mkdir -p "$hooks"
cat >"$hook" <<'EOF'
#!/bin/sh
# flighthook screens-check hook (scripts/screens/install-hook.sh)
# Checks the README screenshots when the pushed commits touch what they show.
zero=0000000000000000000000000000000000000000
paths='ui/ app/ lib/ Cargo.lock scripts/screens/ screens/'
check=no
while read -r _local_ref local_sha _remote_ref remote_sha; do
    [ "$local_sha" = "$zero" ] && continue
    base=$remote_sha
    if [ "$base" = "$zero" ]; then
        # A new branch: compare with where it left main, else check.
        base=$(git merge-base "$local_sha" origin/main 2>/dev/null || true)
        if [ -z "$base" ]; then
            check=yes
            continue
        fi
    fi
    # shellcheck disable=SC2086
    if ! git diff --quiet "$base" "$local_sha" -- $paths 2>/dev/null; then
        check=yes
    fi
done
[ "$check" = yes ] || exit 0
echo "pre-push: checking the README screenshots (make screens-check)"
exec make screens-check
EOF
chmod +x "$hook"
echo "installed $hook"
