#!/usr/bin/env bash
# Builds the GitHub release text of a version from CHANGELOG.md (English) and CHANGELOG.fr.md (French).
# Usage: release-notes.sh v0.5.0 > notes.md   (v0.5 is read as 0.5.0). Fails when a section is missing.
set -euo pipefail
v="${1#v}"
case "$v" in *.*.*) ;; *.*) v="$v.0" ;; esac

section() { # file → the lines between "## [version]" and the next "## [" heading
  awk -v h="## [$v]" 'index($0, h) == 1 { on = 1; next } on && /^## \[/ { exit } on' "$1" | sed -e '/./,$!d'
}
en="$(section CHANGELOG.md)"
fr="$(section CHANGELOG.fr.md)"
[ -n "$en" ] || { echo "::error::No section [$v] in CHANGELOG.md" >&2; exit 1; }
[ -n "$fr" ] || { echo "::error::No section [$v] in CHANGELOG.fr.md" >&2; exit 1; }

cat <<NOTES
$en

## Download
- **Windows**: \`sshOs_${v}_x64-setup.exe\` (installer) or \`sshOs_${v}_x64_en-US.msi\`
- **Linux**: \`.deb\` (Debian/Ubuntu), \`.rpm\` (Fedora/RHEL) or \`.AppImage\` (any distribution)

---

<details>
<summary><b>🇫🇷 Français</b></summary>

$fr

## Téléchargement
- **Windows** : \`sshOs_${v}_x64-setup.exe\` (installeur) ou \`sshOs_${v}_x64_en-US.msi\`
- **Linux** : \`.deb\` (Debian/Ubuntu), \`.rpm\` (Fedora/RHEL) ou \`.AppImage\` (toutes distributions)

</details>
NOTES
