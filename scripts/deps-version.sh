#!/usr/bin/env bash
# Ties the app's version to its dependencies, so that every dependency update
# (including a vulnerability fix) reaches users as a new release.
#
#   scripts/deps-version.sh bump           If the version in Cargo.toml is already
#                                          released (tag v<version> exists) and the
#                                          packages in Cargo.lock differ from that
#                                          release, raise the micro version
#                                          (0.1.5 -> 0.1.6) in Cargo.toml and
#                                          Cargo.lock. Prints the new version, or
#                                          nothing when no bump is needed.
#   scripts/deps-version.sh changes <ref>  List the packages that changed in
#                                          Cargo.lock since <ref> (a tag or commit),
#                                          as Markdown bullets.
#
# CI (.github/workflows/build.yml) runs "bump" on every push to main and commits
# the result; the release that follows makes running apps show the
# "A new version is available" popup.
set -euo pipefail
cd "$(dirname "$0")/.."

self=$(sed -n 's/^name = "\(.*\)"/\1/p' Cargo.toml | head -1)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

# "name version" for every package in a Cargo.lock read from stdin, except this app.
packages() {
    awk -v self="$self" '
        /^\[\[package\]\]/ { name = "" }
        /^name = /    { name = $3; gsub(/"/, "", name) }
        /^version = / { v = $3; gsub(/"/, "", v); if (name != self) print name, v }
    ' | sort -u
}

# Markdown list of what changed between two package lists (files).
describe() {
    local old=$1 new=$2
    comm -23 "$old" "$new" > "$old.gone"
    comm -13 "$old" "$new" > "$new.came"
    awk '
        FNR == NR { gone[$1] = gone[$1] (gone[$1] ? ", " : "") $2; next }
                  { came[$1] = came[$1] (came[$1] ? ", " : "") $2 }
        END {
            for (n in came) print n in gone ? "- " n " " gone[n] " → " came[n] : "- " n " " came[n] " (new)"
            for (n in gone) if (!(n in came)) print "- " n " " gone[n] " (removed)"
        }
    ' "$old.gone" "$new.came" | sort
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

changes_since() {
    git show "$1:Cargo.lock" | packages > "$tmp/old"
    packages < Cargo.lock > "$tmp/new"
    describe "$tmp/old" "$tmp/new"
}

case "${1:-}" in
    changes)
        [ -n "${2:-}" ] || { echo "usage: $0 changes <ref>" >&2; exit 2; }
        changes_since "$2"
        ;;
    bump)
        # Not released yet: this version is the next release already.
        git rev-parse -q --verify "refs/tags/v$version" >/dev/null || exit 0
        list=$(changes_since "v$version")
        [ -n "$list" ] || exit 0

        IFS=. read -r major minor micro <<< "${version%%[-+]*}"
        next="$major.$minor.$((micro + 1))"
        while git rev-parse -q --verify "refs/tags/v$next" >/dev/null; do
            next="$major.$minor.$((${next##*.} + 1))"
        done

        # The first version line is [package]'s (the same one CI reads).
        sed -i "0,/^version = \"$version\"/s//version = \"$next\"/" Cargo.toml
        awk -v self="$self" -v next_v="$next" '
            /^name = / { mine = ($3 == "\"" self "\"") }
            mine && /^version = / { $0 = "version = \"" next_v "\""; mine = 0 }
            { print }
        ' Cargo.lock > "$tmp/lock" && cat "$tmp/lock" > Cargo.lock

        # A <release> entry, so AppStream (software centres, AppImage tools)
        # sees the new version too.
        metainfo=$(ls assets/*.metainfo.xml 2>/dev/null | head -1)
        if [ -n "$metainfo" ] && ! grep -q "<release version=\"$next\"" "$metainfo"; then
            count=$(grep -c . <<< "$list")
            {
                echo "    <release version=\"$next\" date=\"$(date -u +%F)\">"
                echo "      <description>"
                echo "        <p>Dependency updates, including any security fixes:</p>"
                echo "        <ul>"
                head -10 <<< "$list" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/^- \(.*\)/          <li>\1<\/li>/'
                [ "$count" -le 10 ] || echo "          <li>and $((count - 10)) more</li>"
                echo "        </ul>"
                echo "      </description>"
                echo "    </release>"
            } > "$tmp/release"
            sed -i "/<releases>/r $tmp/release" "$metainfo"
        fi

        echo "Dependencies changed since v$version:" >&2
        echo "$list" >&2
        echo "$next"
        ;;
    *)
        sed -n '2,16s/^# \{0,1\}//p' "$0" >&2
        exit 2
        ;;
esac
