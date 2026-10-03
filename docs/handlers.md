# Gestionnaires : contrat, configuration et exemples

Un **gestionnaire** traite les disques d'un système (`psx`, `gc`…) ou d'un type de média (`cdda`, `dvd-video`…).

Chaque gestionnaire a :

- un **manifeste**, qui contient les données : étiquettes, formats, plans de dump, émulateurs ;
- un **exécutable**, qui sait répondre aux verbes `describe`, `play`, `dump-plan` et `convert-plan`.

Le démon ne connaît aucune application. Kodi, mpv, VLC, RetroArch ou DuckStation ne sont que des réglages.

## 1. Quel exécutable est appelé ?

Pour chaque verbe, la première règle qui s'applique gagne :

1. `[handlers.<id>] executable` dans `config.toml` ;
2. `[handler] executable` dans le manifeste ;
3. un exécutable **spécifique** trouvé dans le PATH : `disc-launcher-<id>` pour une console, `disc-launcher-media-<id>` pour un média ;
4. `[handlers.defaults] console` ou `media` dans `config.toml` ;
5. le gestionnaire générique fourni : `disc-launcher-generic` pour les consoles, `disc-launcher-media-generic` pour les médias.

Une valeur peut être une chaîne, valable pour tous les verbes, ou une table par verbe. La clé `"*"` vaut alors pour les autres verbes :

```toml
[handlers.defaults]
console = { play = "disc-launcher-retroarch", "*" = "disc-launcher-generic" }
```

Pour vérifier ce qui sera appelé : `disc-launcher handlers [id]`.

## 2. Contrat

L'exécutable est appelé ainsi :

```sh
<exécutable> <verbe> [options]
```

- **Options :** `--existing <chemin>` pour lancer une copie, `--disc <périphérique>` pour lancer le disque. Les gestionnaires génériques reçoivent en plus `--id <id>`.
- **Entrée :** un document JSON facultatif sur stdin. Un exécutable peut l'ignorer.
- **Sortie :** un document JSON sur stdout, même minimal : `{}`.
- **Codes de retour :**

| Code | Sens |
|---|---|
| `0` | succès |
| `2` | non applicable à ce disque |
| `3` | prérequis manquant |
| `4` | erreur à montrer à l'utilisateur |

`play` doit rendre la main tout de suite : lancez l'application en arrière-plan, ou utilisez `exec` dans un script.

### Variables d'environnement

Toutes sont facultatives pour le gestionnaire. Elles ne sont posées que si l'information est connue.

| Variable | Contenu |
|---|---|
| `DL_VERB`, `DL_ACTION` | verbe appelé : `play`, `describe`, `dump-plan`… |
| `DL_HANDLER`, `DL_SYSTEM` | identifiant du gestionnaire (`psx`, `dvd-video`…) |
| `DL_KIND` | `console` ou `media` |
| `DL_MEDIA` | identifiant du média, pour les médias uniquement |
| `DL_DEVICE`, `DL_DEVNAME` | `/dev/sr0` et `sr0` |
| `DL_MOUNT` | point de montage, s'il y en a un |
| `DL_LABEL` | libellé du volume |
| `DL_TAG`, `DL_CONFIDENCE` | étiquette du classificateur (`video:dvd`) et confiance (`certain`) |
| `DL_MEDIA_TYPE` | type physique : `cd`, `dvd`, `bd` |
| `DL_TRACKS`, `DL_AUDIO_TRACKS`, `DL_DATA_TRACKS`, `DL_SESSIONS` | structure du disque |
| `DL_FINGERPRINT` | empreinte stable du disque |
| `DL_DRIVE_PROFILE` | `standard`, `omnidrive`, `kreon`, `friidump` |
| `DL_SERIAL`, `DL_GAME_ID`, `DL_REGION`, `DL_DISC`, `DL_DISCS`, `DL_KEY` | identité d'un jeu |
| `DL_TITLE` | titre connu |
| `DL_NAME`, `DL_GAME`, `DL_NAME_CONFIDENCE` | nom canonique prévu, nom du jeu, confiance |
| `DL_TARGET`, `DL_FOLDER`, `DL_ROMS_DIR` | fichier prédit, dossier ES-DE, racine des ROMs |
| `DL_EXISTING`, `DL_EXT` | copie à lancer et son extension |
| `DL_CHECKED=1` | le démon a déjà vérifié le disque |
| `DL_PLAY_HANDLER` | la lecture est confiée à un autre exécutable que le générique |
| `DL_INFO` | chemin d'un fichier JSON avec l'identification complète |

Les gabarits de commande (`{…}`) des gestionnaires génériques acceptent ces variables en minuscules, sans le préfixe `DL_` : `{device}`, `{label}`, `{system}`, `{existing}`, `{serial}`…

Les étapes de dump (`[[dump.plan.step]]`) disposent en plus de `{tmp}` (dossier de travail), `{stem}` (nom du fichier sans extension), `{key}` (clé de disque, PS3) et `{sgdevice}` : le périphérique SCSI générique du lecteur (`/dev/sgN`), qu'exige redumper sous Linux. `{device}` reste le périphérique bloc (`/dev/srN`).

Trois familles de gestionnaires (`kind` du manifeste) :

| `kind` | Exemples | Exécutable spécifique cherché | Générique |
| --- | --- | --- | --- |
| `console` | `psx`, `n64` | `disc-launcher-<id>` | `disc-launcher-generic` |
| `media` | `cdda`, `dvd-video`, `dcim` | `disc-launcher-media-<id>` | `disc-launcher-media-generic` |
| `data` | `data-audio`, `data-video` | `disc-launcher-data-<id>` | `disc-launcher-data-generic` |

Pour une cartouche (Retrode), `DL_DEVICE` et `{device}` désignent le fichier ROM sur le volume de la Retrode, et le support (`physical.media`) vaut `cart`.

## 3. Les gestionnaires génériques

### Consoles : `disc-launcher-generic`

Le gestionnaire générique console est entièrement piloté par le manifeste. Il lit :

- les plans de dump `[[dump.plan]]` (détaillés ci-dessous) ;
- la conversion `[convert]` ;
- les émulateurs `[emulators.<nom>]`, qui ont chacun deux commandes :
  - `existing` pour lancer une copie ;
  - `disc` pour lancer le disque ;
  - `existing_<ext>` (facultatif) pour une copie de cette extension, ex. `existing_wad` ;
  - et, au choix, `program` : les noms possibles du programme (le premier trouvé remplace le premier mot des commandes).

#### Plans de dump et replis

Un manifeste peut déclarer plusieurs plans pour un même support. Le générique retient, parmi ceux qui conviennent au support (`media`) et au profil du lecteur (`profiles`), **le premier dont tous les outils sont installés**. L'ordre de déclaration est donc l'ordre de préférence : l'outil exact d'abord, les replis ensuite.

| Clé du plan | Rôle |
| --- | --- |
| `name` | nom affiché (`doctor`, journal) ; par défaut le premier outil |
| `media`, `profiles` | supports et profils de lecteur acceptés (vide : tous) |
| `exact = false` | image utilisable mais non conforme Redump : le journal le signale |
| `outputs`, `verify` | remplacent ceux de `[dump]` pour ce plan |
| `[[dump.plan.step]]` | étapes : `name`, `command`, `helper` (assistant privilégié), `progress` |

Une étape nommée `read` (ou passant par l'assistant) est une lecture du disque : le disque est démonté avant, le tiroir verrouillé pendant, éjecté après. Les commandes s'exécutent dans le dossier de travail `{tmp}`.

Exemple : les manifestes CD fournis déclarent redumper puis cdrdao.

```toml
[[dump.plan]]
media = ["cd"]
[[dump.plan.step]]
name = "read"
command = ["redumper", "disc", "--drive={sgdevice}", "--image-path={tmp}", "--image-name={stem}"]
helper = true

[[dump.plan]]
name = "cdrdao"
media = ["cd"]
exact = false
verify = []
[[dump.plan.step]]
name = "read"
command = ["cdrdao", "read-cd", "--read-raw", "--driver", "generic-mmc-raw", "--device", "{device}", "--datafile", "{stem}.bin", "{stem}.toc"]
[[dump.plan.step]]
name = "cue"
command = ["toc2cue", "{stem}.toc", "{stem}.cue"]
```

`disc-launcher doctor` indique le plan de repli utilisé (« dump de repli : cd : cdrdao »), ou les outils manquants du plan préféré si aucun plan n'est utilisable. La section facultative `[requirements] commands = […]` liste des outils exigés en plus des plans.

```toml
# ~/.config/disc-launcher/handlers/psx.toml (fusionné avec le manifeste fourni)
[emulator]
default = "duckstation"
[emulators.duckstation]
existing = ["duckstation-qt", "-batch", "-fullscreen", "--", "{existing}"]
```

### Médias : `disc-launcher-media-generic`

Le gestionnaire générique média revérifie d'abord le disque : il refuse s'il s'agit d'un jeu. Il choisit ensuite un **lecteur** dans `[media.players]` :

1. le lecteur de `[handlers.<média>] player`, ou à défaut celui de `[media] player` ;
2. sinon le premier lecteur installé dans l'ordre de `auto_order`.

Pour chaque lecteur, la commande utilisée est celle du média s'il y en a une, sinon `default`, sinon `command`. Une commande dont une variable est vide, par exemple `{mount}` sans montage, est ignorée et on passe au lecteur suivant.

Kodi est un lecteur parmi d'autres. Il est implémenté par `disc-launcher-player-kodi`, qui appelle l'addon `script.disc.import` par JSON-RPC, ou démarre Kodi. Il est facultatif.

## 4. Exemples

### VLC pour tous les médias, mpv pour les CD audio

```toml
[media]
player = "vlc"

[handlers.cdda]
player = "mpv"
```

Plein écran et titre du disque, par un lecteur personnalisé :

```toml
[media.players.vlc-fs]
name = "VLC (plein écran)"
dvd-video = ["vlc", "--fullscreen", "--meta-title={label}", "dvd://{device}"]
bluray-video = ["vlc", "--fullscreen", "bluray://{device}"]

[media]
player = "vlc-fs"
```

Le même résultat avec un script spécifique, déposé dans le PATH. Il est prioritaire sur le générique :

```sh
#!/bin/sh
# ~/.local/bin/disc-launcher-media-dvd-video
case "$DL_VERB" in
  describe) echo '{"actions":{"play-disc":true},"player_name":"VLC"}' ;;
  play)     setsid vlc --fullscreen "dvd://$DL_DEVICE" >/dev/null 2>&1 &
            echo '{}' ;;
  *)        exit 2 ;;
esac
```

### Lutris pour les jeux PC sur CD/DVD

L'identification des jeux PC est heuristique : il faut l'autoriser avec `[handlers.windows] allow_heuristic = true`. On confie ensuite la lecture à un script qui associe le libellé du disque à un jeu Lutris. La syntaxe des URI Lutris est à vérifier selon votre version.

```toml
[handlers.windows]
allow_heuristic = true
executable = { play = "~/bin/dl-lutris", describe = "~/bin/dl-lutris", "*" = "disc-launcher-generic" }
```

```sh
#!/bin/sh
# ~/bin/dl-lutris — table « LIBELLÉ_DU_DISQUE slug-lutris » dans ~/.config/dl-lutris.txt
slug=$(awk -v l="$DL_LABEL" '$1 == l { print $2 }' ~/.config/dl-lutris.txt)
case "$DL_VERB" in
  describe)
    [ -n "$slug" ] && echo '{"actions":{"play-disc":true,"play-existing":false,"dump":true}}' \
                   || echo '{"actions":{"play-disc":false,"dump":true}}' ;;
  play)
    [ -n "$slug" ] || exit 2
    # installé : lancer ; sinon : ouvrir l'installation (le disque reste monté sur $DL_MOUNT)
    if lutris --list-games 2>/dev/null | grep -q "$slug"; then
      setsid lutris "lutris:rungame/$slug" >/dev/null 2>&1 &
    else
      setsid lutris "lutris:$slug" >/dev/null 2>&1 &
    fi
    echo '{}' ;;
  *) exit 2 ;;
esac
```

### RetroArch pour toutes les consoles

`disc-launcher-retroarch` est fourni. Il choisit le cœur libretro installé, d'après une liste de préférence et les fichiers `.info` (base de données, extensions acceptées). Il délègue le dump et la conversion au générique, il peut donc remplacer celui-ci entièrement :

```toml
[handlers.defaults]
console = "disc-launcher-retroarch"

[retroarch]
command = ["flatpak", "run", "org.libretro.RetroArch"]
cores_dir = "~/.var/app/org.libretro.RetroArch/config/retroarch/cores"
[retroarch.cores]
saturn = "yabasanshiro"     # forcer un cœur
```

### PlayStation : un lanceur qui choisit selon l'extension et la région

Le lanceur PS1 ci-dessous illustre un modèle réutilisable : il choisit l'émulateur d'après l'extension de la copie (`DL_EXT`) et adapte un réglage d'après la région du disque (`DL_REGION`).

- `.chd`, `.cue` et les listes `.m3u` des jeux multi-disques vont à DuckStation, en plein écran ;
- les `.pbp` (copies issues d'autres outils) vont à RetroArch avec le cœur PCSX ReARMed ;
- pour un disque européen, le script exporte une variable que votre configuration DuckStation peut exploiter (ici, à titre d'exemple, le choix d'un BIOS PAL).

Lancer le disque lui-même (`play-disc`) reste confié au générique, grâce à la table par verbe :

```toml
[handlers.psx]
executable = { play = "~/bin/dl-psx", "*" = "disc-launcher-generic" }
```

```sh
#!/bin/sh
# ~/bin/dl-psx
[ "$DL_VERB" = play ] || exit 2
# Disque physique : on laisse faire le gestionnaire générique.
[ -z "$DL_EXISTING" ] && exec disc-launcher-generic play --id psx --disc "$DL_DEVICE"
f=$DL_EXISTING
case "$DL_REGION" in
  Europe*) export DUCKSTATION_BIOS=scph5502.bin ;;   # exemple : réglage propre à votre installation
esac
case "$DL_EXT" in
  chd|cue|m3u) setsid duckstation-qt -batch -fullscreen -- "$f" >/dev/null 2>&1 & ;;
  pbp)         setsid retroarch -L ~/.config/retroarch/cores/pcsx_rearmed_libretro.so "$f" >/dev/null 2>&1 & ;;
  *)           exit 2 ;;
esac
echo '{}'
```

Comme la lecture est confiée à un autre exécutable, le générique répond à `describe` en considérant la lecture disponible (variable `DL_PLAY_HANDLER`).

### Un lanceur qui lit la configuration d'EmulationStation (esquisse)

Écrire un tel lanceur est un projet à part : il n'est pas fourni. Le système de gestionnaires lui offre déjà tout ce qu'il lui faut :

- **Déclaration :** `[handlers.defaults] console = { play = "esde-launch", "*" = "disc-launcher-generic" }`.
- **Données reçues :** `DL_SYSTEM`, qui est aussi le nom du dossier ES-DE, `DL_FOLDER`, `DL_EXISTING`, `DL_EXT` et `DL_ROMS_DIR`.
- **Travail du lanceur :**
  - lire `es_systems.xml` puis `es_find_rules.xml` ;
  - choisir la `<command>` du système, celle par défaut ou l'alternative préférée ;
  - remplacer `%ROM%`, `%EMULATOR_…%` et `%CORE_…%` ;
  - lancer la commande.

```python
#!/usr/bin/env python3
# esde-launch — esquisse
import os, sys, subprocess, xml.etree.ElementTree as ET
system, rom = os.environ["DL_SYSTEM"], os.environ["DL_EXISTING"]
tree = ET.parse(os.path.expanduser("~/ES-DE/custom_systems/es_systems.xml"))  # ou le fichier fourni par ES-DE
cmd = next(s.find("command").text for s in tree.iter("system") if s.findtext("name") == system)
# … résoudre %EMULATOR_X% / %CORE_X% avec es_find_rules.xml, puis :
cmd = cmd.replace("%ROM%", rom)
subprocess.Popen(cmd, shell=True, start_new_session=True)
print("{}")
```
