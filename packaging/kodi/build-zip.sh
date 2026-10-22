#!/bin/sh
# Archive de l'extension Kodi service.disc-launcher, installable dans Kodi
# (« Installer depuis un fichier zip »), construite depuis son dépôt :
#   packaging/kodi/build-zip.sh [dossier de sortie]      (défaut : dist)
#
# Variables :
#   KODI_ADDON_REPO  dépôt de l'extension (défaut : github.com/flavi1/kodi-disc-launcher)
#   KODI_ADDON_REF   branche ou étiquette (défaut : main)
#   KODI_ADDON_SRC   copie locale déjà présente (pas de clonage)
#   KODI_ADDON_TESTS 0 pour ne pas lancer les tests de l'extension
# Affiche le chemin de l'archive produite.
set -eu
out=$(mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd)
repo=${KODI_ADDON_REPO:-https://github.com/flavi1/kodi-disc-launcher.git}
ref=${KODI_ADDON_REF:-main}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -n "${KODI_ADDON_SRC:-}" ]; then
    cp -r "$KODI_ADDON_SRC" "$work/addon"
else
    git clone -q --depth 1 --branch "$ref" "$repo" "$work/addon" >&2
fi
cd "$work/addon"
[ -f addon.xml ] || { echo "addon.xml introuvable dans $repo ($ref)" >&2; exit 1; }
if [ "${KODI_ADDON_TESTS:-1}" != 0 ] && [ -d tests ]; then
    python3 -m unittest discover -s tests >&2
fi
zip=$(./build-zip.sh | tail -n 1)
cp "$zip" "$out/"
echo "$out/$(basename "$zip")"
