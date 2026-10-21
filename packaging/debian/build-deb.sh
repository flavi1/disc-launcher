#!/bin/sh
# Construit le paquet Debian/Ubuntu (.deb) à partir d'une compilation déjà faite
# (`cargo build --release`). Usage : packaging/debian/build-deb.sh [dossier de sortie]
# Pas de debhelper : une arborescence installée par « make install », un
# répertoire DEBIAN, puis dpkg-deb.
set -eu
cd "$(dirname "$0")/../.."
out=${1:-dist}
# VERSION : version imposée (ex. construction de développement 0.5.0+git…).
version=${VERSION:-$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)}
arch=$(dpkg --print-architecture)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

make install DESTDIR="$root" PREFIX=/usr LIBEXECDIR=/usr/libexec SYSCONFDIR=/etc \
    POLKITRULESDIR=/usr/share/polkit-1/rules.d >/dev/null
# Documentation : emplacement Debian, avec la licence comme fichier copyright.
mv "$root/usr/share/doc/disc-launcher/LICENSE" "$root/usr/share/doc/disc-launcher/copyright"

mkdir -p "$root/DEBIAN"
size=$(du -sk --exclude=DEBIAN "$root" | cut -f1)
sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$arch/" -e "s/@SIZE@/$size/" packaging/debian/control.in > "$root/DEBIAN/control"
install -m 755 packaging/debian/postinst packaging/debian/postrm "$root/DEBIAN/"
# Fichiers de configuration conservés lors des mises à jour.
( cd "$root" && find etc -type f | sed 's|^|/|' ) > "$root/DEBIAN/conffiles"

mkdir -p "$out"
dpkg-deb --root-owner-group -Zxz --build "$root" "$out/disc-launcher_${version}_${arch}.deb"
