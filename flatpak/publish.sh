#!/usr/bin/env bash
# Turns a flatpak-builder repo into the signed update site served on GitHub
# Pages, and builds the standalone bundle attached to releases.
#
#   flatpak/publish.sh REPO SITE_DIR BUNDLE GPG_KEY_ID [GPG_HOMEDIR]
#
# REPO is the --repo directory flatpak-builder exported to. It is moved to
# SITE_DIR/repo, next to the .flatpakrepo and .flatpakref files that point
# users at it. Installing either the .flatpakref or the bundle adds the
# remote, so `flatpak update` picks up new releases.
set -euo pipefail

repo=$1 site=$2 bundle=$3 key=$4
homedir=${5:-${GNUPGHOME:-$HOME/.gnupg}}

app_id=io.github.dukko.DrukhalManager
branch=master
base_url=https://dukko.github.io/DrukhalManager
here=$(dirname "$(readlink -f "$0")")
pubkey=$here/drukhal-manager.gpg
runtime_repo=https://dl.flathub.org/repo/flathub.flatpakrepo

sign=(--gpg-sign="$key" --gpg-homedir="$homedir")

# Sign every commit (app, Debug extension, appstream): remotes verify them all.
for ref in $(ostree refs --repo="$repo"); do
  ostree gpg-sign --repo="$repo" --gpg-homedir="$homedir" "$ref" "$key"
done
flatpak build-update-repo "${sign[@]}" --prune --generate-static-deltas \
  --title=DrukhalManager --default-branch="$branch" "$repo"

flatpak build-bundle --repo-url="$base_url/repo/" --runtime-repo="$runtime_repo" \
  --gpg-keys="$pubkey" "$repo" "$bundle" "$app_id" "$branch"

mkdir -p "$site"
rm -rf "$site/repo"
mv "$repo" "$site/repo"

gpg_b64=$(base64 -w0 "$pubkey")

cat > "$site/drukhal-manager.flatpakrepo" <<EOF
[Flatpak Repo]
Title=DrukhalManager
Url=$base_url/repo/
Homepage=https://github.com/Dukko/DrukhalManager
Comment=Mod manager for Stellaris
GPGKey=$gpg_b64
EOF

cat > "$site/$app_id.flatpakref" <<EOF
[Flatpak Ref]
Name=$app_id
Branch=$branch
Title=DrukhalManager
Url=$base_url/repo/
SuggestRemoteName=drukhal-manager
RuntimeRepo=$runtime_repo
IsRuntime=false
GPGKey=$gpg_b64
EOF

cat > "$site/index.html" <<EOF
<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width">
<title>DrukhalManager Flatpak repository</title>
<h1>DrukhalManager Flatpak repository</h1>
<p>Install with automatic updates:</p>
<pre>flatpak install --user $base_url/$app_id.flatpakref</pre>
<p>Then <code>flatpak update</code> (or your software centre) keeps it up to date.
See <a href="https://github.com/Dukko/DrukhalManager">the project</a>.</p>
EOF
