#!/usr/bin/env bash
# Open a winget update pull request for one published streamtop release.
set -euo pipefail

: "${VERSION:?VERSION is required}"
: "${WINGET_TOKEN:?WINGET_TOKEN is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "::error::version must be a release version without a v prefix"
  exit 1
fi

export GITHUB_TOKEN="$WINGET_TOKEN"
export GH_TOKEN="$WINGET_TOKEN"
export KOMAC_FORK_OWNER="Jorji49"

asset="streamtop-windows-x86_64-${VERSION}.zip"
assets="$(gh release view "v${VERSION}" --repo "$GITHUB_REPOSITORY" --json assets --jq '.assets[].name')"
if ! printf '%s\n' "$assets" | grep -Fxq "$asset"; then
  echo "::error::GitHub release v${VERSION} has no ${asset} asset"
  exit 1
fi

url="https://github.com/${GITHUB_REPOSITORY}/releases/download/v${VERSION}/${asset}"
notes="https://github.com/${GITHUB_REPOSITORY}/releases/tag/v${VERSION}"

work="$(mktemp -d)"
archive="komac-2.17.2-x86_64-unknown-linux-gnu.tar.gz"
expected="b03b020eec98acfb0b53e284a6e21815b884be33de6785b97e7a120f0e748966"
curl --retry 3 --retry-delay 2 -fsSL \
  -o "${work}/${archive}" \
  "https://github.com/russellbanks/Komac/releases/download/v2.17.2/${archive}"
actual="$(sha256sum "${work}/${archive}" | awk '{print $1}')"
if [[ "$actual" != "$expected" ]]; then
  echo "::error::komac 2.17.2 archive checksum mismatch"
  exit 1
fi
tar -xzf "${work}/${archive}" -C "$work"
komac="${work}/komac"
chmod +x "$komac"

if ! "$komac" list-versions Jorji49.streamtop; then
  echo "::error::Jorji49.streamtop is not in the winget community repository yet. Merge the initial package, then run this workflow again with the release version."
  exit 1
fi

"$komac" sync-fork

out="${work}/manifests"
"$komac" update Jorji49.streamtop \
  --version "$VERSION" \
  --urls "$url" \
  --release-notes-url "$notes" \
  --output "$out" \
  --exit

installers=()
if [[ -d "$out" ]]; then
  mapfile -t installers < <(find "$out" -name 'Jorji49.streamtop.installer.yaml' | sort)
fi
if [[ ${#installers[@]} -eq 0 ]]; then
  existing="$(gh pr list --repo microsoft/winget-pkgs --state open \
    --search "Jorji49.streamtop version ${VERSION} in:title" \
    --json url,title --jq '.[] | .url + " " + .title')"
  if [[ -z "$existing" ]]; then
    echo "::error::komac wrote no installer manifest for ${VERSION}, and no open winget pull request for that version was found."
    exit 1
  fi
  echo "::notice title=WinGet already open::An open winget pull request for Jorji49.streamtop ${VERSION} already exists, so no new pull request was created."
  printf '%s\n' "$existing"
  exit 0
fi
if [[ ${#installers[@]} -ne 1 ]]; then
  echo "::error::expected one installer manifest, found ${#installers[@]}"
  exit 1
fi

python3 scripts/winget_manifest.py "${installers[0]}" "$VERSION"

locale="$(find "$out" -name 'Jorji49.streamtop.locale.en-US.yaml' -print -quit)"
if [[ -z "$locale" ]] || ! grep -Fq "ReleaseNotesUrl: ${notes}" "$locale"; then
  echo "::error::release notes URL was not set to ${notes}"
  exit 1
fi

"$komac" submit "$out" --yes
"$komac" cleanup --only-merged --all
