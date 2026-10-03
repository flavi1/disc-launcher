# Installer les outils de dump (Ubuntu, Arch)

Ces outils ne servent qu'à **dumper** des disques de jeu ; leur rôle est
présenté dans le [README](../README.md#outils-externes). Installez seulement
ceux des systèmes qui vous intéressent, puis vérifiez avec :

```sh
disc-launcher doctor        # colonne « manque : … » par système
```

| Outil | Ubuntu (24.04 et suivantes) | Arch |
| --- | --- | --- |
| chdman | paquet `mame-tools` | paquet `mame-tools` |
| redumper | binaire des versions GitHub | AUR `redumper-bin` |
| dolphin-tool | Flatpak de Dolphin + petit script | paquet `dolphin-emu-tool` |
| extract-xiso | compilation (cmake) | AUR `extract-xiso` |
| cdrdao (repli CD) | paquet `cdrdao` | paquet `cdrdao` |

## Ubuntu

### chdman, et cdrdao (repli sans redumper)

```sh
sudo apt install mame-tools cdrdao
```

### redumper

redumper n'est pas empaqueté par Ubuntu. Les versions publiées sur GitHub
contiennent un exécutable Linux lié statiquement, sans dépendance :

1. téléchargez l'archive `redumper-…-linux-x64.zip` de la dernière version :
   <https://github.com/superg/redumper/releases/latest> ;
2. installez l'exécutable :

```sh
cd ~/Téléchargements
unzip redumper-*-linux-x64.zip
sudo install -o root -g root -m 755 redumper-*-linux-x64/bin/redumper /usr/local/bin/redumper
redumper --help | head -n 3
```

L'exécutable doit appartenir à root et ne pas être modifiable par d'autres :
l'assistant privilégié refuse sinon de le lancer.

redumper publie une version presque chaque semaine. Pour suivre la dernière
automatiquement (dans un script de mise à jour, par exemple) :

```bash
# redumper
# ========
# Binaire statique, installé dans /usr/local/bin et propriété de root :
# l'assistant privilégié de disc-launcher refuse un exécutable modifiable par
# l'utilisateur (donc pas ~/.local/bin). Dépendances : curl, jq, unzip.

echo "Vérification de la dernière version de redumper..."
REDUMPER_JSON=$(curl -sL -H "Accept: application/vnd.github+json" "https://api.github.com/repos/superg/redumper/releases/latest")
REDUMPER_TAG=$(echo "$REDUMPER_JSON" | jq -r '.tag_name // empty')
REDUMPER_URL=$(echo "$REDUMPER_JSON" | jq -r '.assets[]? | select(.name | test("linux-x64\\.zip$")) | .browser_download_url' | head -n 1)
REDUMPER_STAMP=/usr/local/share/redumper.version

if [ -z "$REDUMPER_URL" ]; then
    echo "Erreur : impossible de trouver l'archive Linux de redumper."
elif [ "$(cat "$REDUMPER_STAMP" 2>/dev/null)" = "$REDUMPER_TAG" ] && [ -x /usr/local/bin/redumper ]; then
    echo "redumper est déjà à jour ($REDUMPER_TAG)."
else
    echo "Téléchargement depuis : $REDUMPER_URL"
    REDUMPER_TMP=$(mktemp -d)
    if curl -sfL "$REDUMPER_URL" -o "$REDUMPER_TMP/redumper.zip" \
        && unzip -q "$REDUMPER_TMP/redumper.zip" -d "$REDUMPER_TMP" \
        && REDUMPER_BIN=$(find "$REDUMPER_TMP" -type f -path '*/bin/redumper' | head -n 1) \
        && [ -n "$REDUMPER_BIN" ] \
        && sudo install -o root -g root -m 755 "$REDUMPER_BIN" /usr/local/bin/redumper \
        && echo "$REDUMPER_TAG" | sudo tee "$REDUMPER_STAMP" >/dev/null; then
        echo "redumper a été mis à jour : $REDUMPER_TAG"
    else
        echo "Erreur : mise à jour de redumper impossible."
    fi
    rm -rf "$REDUMPER_TMP"
fi
```

### dolphin-tool

`dolphin-tool` fait partie de Dolphin, mais les paquets Ubuntu ou les PPA ne le
fournissent pas toujours. Vérifiez d'abord :

```sh
command -v dolphin-tool || dpkg -S dolphin-tool
```

S'il manque, le Flatpak officiel de Dolphin le contient. Installez-le, puis
créez un petit script `dolphin-tool` qui l'appelle :

```sh
sudo apt install flatpak
flatpak remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install flathub org.DolphinEmu.dolphin-emu

sudo tee /usr/local/bin/dolphin-tool >/dev/null <<'EOF'
#!/bin/sh
# Le Flatpak de Dolphin ne voit les fichiers qu'en lecture : on lui ouvre le
# dossier personnel (~/ROMs) en écriture pour cet appel.
exec flatpak run --filesystem=home --command=dolphin-tool org.DolphinEmu.dolphin-emu "$@"
EOF
sudo chmod 755 /usr/local/bin/dolphin-tool
dolphin-tool --help | head -n 3
```

Si votre dossier de ROMs n'est pas dans votre dossier personnel, remplacez
`--filesystem=home` par `--filesystem=/chemin/des/ROMs`.

### extract-xiso

Pas de paquet Ubuntu : on le compile (une minute).

```sh
sudo apt install git cmake build-essential
git clone https://github.com/XboxDev/extract-xiso.git
cmake -S extract-xiso -B extract-xiso/build -DCMAKE_BUILD_TYPE=Release
cmake --build extract-xiso/build
sudo install -m 755 extract-xiso/build/extract-xiso /usr/local/bin/extract-xiso
```

## Arch Linux

Paquets officiels :

```sh
sudo pacman -S mame-tools dolphin-emu-tool cdrdao
```

Paquets AUR (ici avec `yay` ; toute autre aide AUR convient, ou `makepkg`) :

```sh
yay -S redumper-bin extract-xiso
```

`redumper-git` compile la dernière version au lieu de prendre le binaire
publié ; il demande clang et cmake récents.

## Après l'installation

### Dumps sans mot de passe : groupe disc-launcher

Les lectures brutes passent par l'assistant privilégié (`pkexec`). `sudo make
install` crée le groupe `disc-launcher` ; ses membres dumpent sans mot de
passe, les autres utilisateurs doivent s'authentifier (mot de passe
administrateur, mémorisé quelques minutes) :

```sh
sudo usermod -aG disc-launcher "$USER"   # puis déconnexion et reconnexion
disc-launcher doctor                     # ligne « groupe »
```

L'assistant cherche redumper dans `/usr/local/bin` puis `/usr/bin` (profil
`redumper-disc`). Installé ailleurs : redéfinissez le profil dans
`/etc/disc-launcher/helper-tools.toml` (exemple dans le fichier).

### Accès au lecteur

redumper n'utilise pas `/dev/srN` mais le périphérique SCSI générique du même
lecteur (`/dev/sgN`), que disc-launcher trouve tout seul. Sur un bureau
récent, la session active y a accès automatiquement (ACL posée par logind) :

```sh
ls /sys/block/sr0/device/scsi_generic/     # → sg1, par exemple
getfacl /dev/sg1 | grep "user:$USER"       # → user:<vous>:rw-
```

Sans ACL (système sans logind), ajoutez-vous au groupe du lecteur, puis
reconnectez-vous : `sudo usermod -aG cdrom $USER` (Ubuntu) ou
`sudo usermod -aG optical $USER` (Arch).

Le disque doit aussi être démonté pendant la lecture : disc-launcher le
démonte (via udisks2) avant de lancer redumper.

### Lecteur reconnu par redumper

redumper ajuste sa lecture au modèle du lecteur (décalage, ordre des données
C2). Pour savoir s'il connaît le vôtre :

```sh
redumper --list-all-drives | grep -i "$(cat /sys/block/sr0/device/model)"
```

S'il n'y figure pas, ou seulement avec un autre micrologiciel, redumper affiche
« drive not found in the database » et applique une configuration générique
qui **ne lit pas la pré-zone (pre-gap)** du disque. Un disque dont les données
débordent avant le secteur 0 échoue alors en fin de lecture :

```text
warning: incomplete pre-gap (session: 1, unavailable: 150/150)
errors detected, track: 1, sectors: {SKIP: 2, C2: 0}, …
error: data errors detected, unable to continue
```

C'est fréquent sur PlayStation (décalage d'écriture de −647 échantillons, soit
un peu plus d'un secteur avant le début). La solution est de donner à redumper
les réglages de votre modèle, relevés dans `redumper --list-all-drives` pour un
micrologiciel voisin, en les ajoutant au lecteur dans la configuration :

```toml
# ~/.config/disc-launcher/config.toml
# Exemple : LG WH16NS40 en micrologiciel 1.02 (redumper connaît le 1.05)
[drives."/dev/sr0"]
redumper_args = ["--drive-type=GENERIC", "--drive-read-offset=6", "--drive-c2-shift=0",
                 "--drive-pregap-start=-135", "--drive-read-method=BE",
                 "--drive-sector-order=DATA_C2_SUB"]
```

Ces options sont ajoutées à chaque appel de redumper pour ce lecteur ; elles
font partie des options acceptées par le profil `redumper-disc` de l'assistant. La ligne de la base
de redumper se lit ainsi : fabricant, modèle, micrologiciel, …, décalage de
lecture (`+6`), décalage C2 (`0`), début de pré-zone (`-135`), méthode de
lecture (`BE`), ordre des secteurs (`DATA_C2_SUB`), type (`GENERIC`).
`redumper drive::test --drive=/dev/sgN --verbose` (en root) aide à vérifier un
lecteur inconnu.
