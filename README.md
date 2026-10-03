# disc-launcher

Quand un disque optique est inséré, disc-launcher l'identifie et propose quoi en faire :

- **Médias :** un CD audio, un DVD ou un Blu-ray vidéo est **lu** avec le lecteur multimédia de votre choix (Kodi, mpv, VLC…).
- **Jeux :** un jeu de console est **lancé** dans son émulateur, ou **dumpé** dans `~/ROMs/<système>/` avec les noms de dossiers d'ES-DE.
- **Disques de fichiers :** un disque ne contenant que de la musique (FLAC, MP3…), que de la vidéo (MKV, DivX…) ou des photos d'appareil (dossier `DCIM`) est lu ou affiché.
- **Cartouches :** avec une **Retrode**, la cartouche insérée (Nintendo 64, Super Nintendo, Mega Drive, Game Boy, Game Boy Color, Game Boy Advance, Master System, Game Gear) est identifiée sans être dumpée, puis jouée depuis `~/ROMs` (dumpée au besoin).

Le nom du fichier final est prédit avant toute lecture. Si le jeu est déjà dans la collection, même renommé ou déplacé, l'action proposée devient « Lancer la copie ». Un autre disque de données ne déclenche aucune proposition (les CD-ROM de jeux PC, reconnus à leur `AUTORUN.INF` ou `SETUP.EXE`, sont ignorés par défaut : voir `[handlers.windows] allow_heuristic` et `[policy] windows`).

Dépôt : <https://github.com/flavi1/disc-launcher>. Contrat et exemples des gestionnaires : [docs/handlers.md](docs/handlers.md).

## Principes

- **Peu de dépendances.** Rust et sa bibliothèque standard, plus libsqlite3, appelée directement. Les formats JSON, TOML et XML ainsi que le protocole D-Bus sont implémentés dans le projet.
- **Indépendant du système d'init.** systemd n'est pas nécessaire : la supervision des tâches et les journaux sont gérés en interne, et le démon démarre par XDG Autostart. Des exemples pour systemd, OpenRC, runit et s6 sont fournis dans `contrib/`.
- **Indépendant du bureau.** Les propositions passent par les notifications freedesktop, avec des boutons d'action. Sans serveur de notifications compatible, kdialog, zenity ou yad prennent le relais.
- **Indépendant des applications.** Les émulateurs et les lecteurs multimédias ne sont que des réglages. Chaque système ou média peut être confié à un exécutable de votre choix (voir « Gestionnaires »).

## Compilation et installation

Prérequis de compilation : Rust ≥ 1.75 avec cargo (testé avec 1.97), un compilateur C pour l'édition de liens, et le paquet de développement de SQLite.

| Distribution | Commande |
| --- | --- |
| Debian, Ubuntu, Mint | `sudo apt install build-essential libsqlite3-dev` puis Rust (voir ci-dessous) |
| Fedora | `sudo dnf install gcc sqlite-devel cargo` |
| Arch, Manjaro | `sudo pacman -S base-devel sqlite rust` |
| openSUSE | `sudo zypper install gcc sqlite3-devel cargo` |

Rust fourni par la distribution convient s'il est assez récent (`cargo --version`). Sinon, ou sur Debian/Ubuntu, installez-le avec rustup, sans root :

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

```sh
make                      # cargo build --release
make test                 # tests unitaires
python3 tests/e2e.py target/release   # tests de bout en bout (outils simulés)
sudo make install         # PREFIX=/usr/local par défaut ; PREFIX=/usr pour un paquet
```

Après une mise à jour, inutile de supprimer quoi que ce soit : `make install` remplace les exécutables et les manifestes de `$(PREFIX)/share/disc-launcher/`, et conserve la configuration de `/etc/disc-launcher/` (une nouvelle version de `helper-tools.toml` est alors déposée à côté, en `.dist`). Le démon déjà lancé se relance de lui-même sur le nouvel exécutable dès qu'il est au repos (aucun dump suivi, aucun disque en attente de réponse).

`sudo make install` ne recompile pas : il copie ce que `make` a produit. Lancez
donc `make` d'abord, sans sudo (sous sudo, le `cargo` de rustup n'est pas dans
le PATH).

L'installation dépose :

| Élément | Emplacement |
| --- | --- |
| Démon, commande, classificateur | `$(PREFIX)/bin` : `disc-launcherd`, `disc-launcher`, `disc-identify`, `disc-launcher-job` |
| Gestionnaires génériques | `disc-launcher-generic` (consoles, cartouches), `disc-launcher-media-generic` (médias), `disc-launcher-data-generic` (disques de fichiers) |
| Gestionnaires et lecteurs facultatifs | `disc-launcher-retroarch`, `disc-launcher-player-kodi`, `disc-launcher-resolve-serials` |
| Assistant privilégié | `$(PREFIX)/libexec/disc-launcher/disc-launcher-helper` et sa politique polkit |
| Manifestes, signatures, profils de lecteurs | `$(PREFIX)/share/disc-launcher/` |
| Configuration | `/etc/disc-launcher/config.toml` et `helper-tools.toml` |
| Démarrage du démon | `/etc/xdg/autostart/disc-launcherd.desktop` (voir « Démarrage du démon ») |
| Exemples de services, non officiels (systemd, OpenRC, runit, s6) | `$(PREFIX)/share/doc/disc-launcher/contrib/` |

Variantes :

- `sudo make install-native` ajoute les « coquilles » natives Solid et GIO (« Lire le disque »), pour le mode `hybride`.
- `make install-user` installe dans `~/.local`, sans root et sans assistant privilégié.

## Outils externes

disc-launcher identifie les disques et propose les actions lui-même, mais il ne lit ni ne convertit les images : il délègue à des outils spécialisés, appelés comme programmes séparés. Aucun n'est nécessaire pour les médias (CD audio, DVD, Blu-ray) ni pour lancer un jeu déjà dumpé ; ils le sont pour **dumper**. `disc-launcher doctor` indique, pour chaque système, les outils qui manquent.

| Outil | Rôle | Systèmes concernés |
| --- | --- | --- |
| **redumper** | Lit le disque au plus près du support (secteurs bruts, sous-canaux, décalage du lecteur) et produit l'image `.bin`/`.cue` ou `.iso` de référence Redump. C'est l'étape « lecture » de presque tous les dumps. | tous les systèmes sur disque |
| **chdman** (paquet MAME) | Compresse l'image en `.chd`, sans perte, et vérifie un `.chd` existant. | CD (PlayStation, Saturn, Mega-CD, PC Engine CD, Dreamcast…), PS2 |
| **dolphin-tool** (Dolphin) | Convertit l'image GameCube ou Wii en `.rvz`, sans perte et bien plus compacte. | GameCube, Wii |
| **cdrdao** | Repli pour les CD sans redumper (voir ci-dessous). | systèmes sur CD |
| **extract-xiso** | Réécrit l'image Xbox au format XISO, sans la partition vidéo inutile, attendu par xemu. | Xbox |

**Sans redumper.** Chaque manifeste peut déclarer plusieurs plans de dump ; le premier dont les outils sont installés est utilisé. Les manifestes fournis prévoient ces replis :

| Support | Repli | Résultat |
| --- | --- | --- |
| CD (PlayStation, Saturn, Mega-CD, PC Engine CD…) | **cdrdao** + `toc2cue`, puis chdman | jouable (pistes audio, vidéos et secteurs complets conservés), mais non conforme Redump : pas de vérification par la base, protections par sous-canal (LibCrypt) perdues |
| DVD (PlayStation 2, jeux PC) | **dd** | identique à redumper si la lecture est sans erreur (un DVD n'a ni pistes audio ni sous-canal) |

Pas de repli pour GameCube, Wii, Xbox, Xbox 360 (lecteurs spéciaux), Dreamcast (GD-ROM illisible par un lecteur ordinaire) ni PS3. `disc-launcher doctor` indique le plan de repli retenu.

Autres outils, pour des cas particuliers : `ps3dec` et `xorriso` (PS3), `friidump` (GameCube/Wii sur lecteurs compatibles). Les émulateurs eux-mêmes (DuckStation, PCSX2, Dolphin, RetroArch…) ne servent qu'à jouer.

Installation sous Ubuntu et Arch : [`docs/dependances.md`](docs/dependances.md).

## Démarrage du démon

Le démon `disc-launcherd` tourne sous votre compte, dans votre session graphique : il a besoin de votre bus D-Bus (notifications) et de votre affichage (dialogues, émulateurs, lecteurs). Ce n'est pas un service système.

### XDG Autostart (tous systèmes d'init)

`make install` dépose `/etc/xdg/autostart/disc-launcherd.desktop`. Les bureaux qui suivent la spécification XDG Autostart (KDE Plasma, GNOME, Xfce, LXQt, Cinnamon, MATE…) lancent le démon à l'ouverture de session, sans dépendre de systemd, OpenRC ou autre. Après l'installation :

- déconnectez-vous puis reconnectez-vous ;
- ou lancez-le tout de suite, détaché du terminal : `setsid -f disc-launcherd` (une seconde instance s'arrête aussitôt, sans erreur).

Contrôle :

```sh
disc-launcher doctor             # démon en marche ?
disc-launcher status             # lecteurs et disques vus par le démon
disc-launcher log -f             # journal (~/.local/state/disc-launcher/log/daemon.log)
disc-launcher reload             # relire la configuration
```

Un gestionnaire de fenêtres sans XDG Autostart (i3, sway, bspwm…) : ajoutez `disc-launcherd` à sa configuration de démarrage (`exec disc-launcherd` pour i3/sway, `~/.xinitrc`…).

### Avec un gestionnaire de services (utilisateurs avertis)

XDG Autostart est la seule méthode prise en charge. Des exemples non officiels pour systemd, OpenRC, runit et s6, avec leurs précautions, sont dans [`contrib/README.md`](contrib/README.md).

### Vérifier l'installation

- `disc-launcher doctor` affiche les capacités détectées, les lecteurs, et pour chaque gestionnaire l'émulateur ou le lecteur retenu ;
- `disc-launcher handlers` affiche l'exécutable retenu pour chaque verbe.

## Médias : lecteurs multimédias

`disc-launcher-media-generic` revérifie d'abord le disque : il refuse s'il s'agit d'un jeu, par exemple la partition vidéo d'un disque Xbox. Il lance ensuite le premier lecteur disponible :

```toml
[media]
player = "auto"                     # ou "vlc", "mpv", "kodi"…
auto_order = ["kodi", "mpv", "vlc"]

[handlers.cdda]
player = "mpv"                      # choix par média

[media.players.vlc]
name = "VLC"
dvd-video = ["vlc", "dvd://{device}"]
default = ["vlc", "{mount}"]
```

**Kodi** est un lecteur comme les autres, fourni par `disc-launcher-player-kodi`. Deux cas :

- **Kodi tourne.** L'addon `script.disc.import` est déclenché par JSON-RPC avec `Addons.ExecuteAddon`, ce qui équivaut à `RunScript(...)`. Si JSON-RPC est fermé, l'appel passe par l'EventServer.
- **Kodi ne tourne pas.** Kodi est démarré et l'addon détecte le disque lui-même. Avec `startup = "rpc"`, disc-launcher attend que Kodi réponde, puis déclenche l'addon.

Les réglages de ce lecteur sont dans `[media.players.kodi]`. Côté Kodi, activez dans *Paramètres › Services › Contrôle* l'option « Autoriser le contrôle par des applications sur ce système ».

## Disques de fichiers : musique, vidéo, photos

Un disque de données est examiné (dossiers compris) quand rien d'autre ne l'a identifié :

| Contenu | Gestionnaire | Lecture |
| --- | --- | --- |
| uniquement de la musique (`flac`, `mp3`, `ogg`, `opus`, `m4a`, `wav`…) | `data-audio` | liste de lecture de tous les morceaux, dans l'ordre naturel |
| uniquement de la vidéo (`mkv`, `avi`/DivX, `mp4`, `mpg`, `webm`…) | `data-video` | liste de lecture des fichiers vidéo |
| un dossier `DCIM` (appareil photo, norme DCF) | `dcim` | visionneuse d'images du bureau, ou diaporama Kodi |

Pochettes, sous-titres, `autorun.inf`, `.nfo`, `.cue`, `.m3u`… sont ignorés ; un seul programme ou document (`.exe`, `.pdf`…) suffit à écarter le disque. `data-audio` et `data-video` sont servis par `disc-launcher-data-generic` : il monte le disque au besoin (udisks2), écrit la liste M3U et la confie au lecteur choisi dans `[media]`, avec les gabarits `{playlist}`, `{first}`, `{count}`. Kodi les ouvre par JSON-RPC (`Player.Open`). Comme pour les médias, `[handlers.data-audio] player = "mpv"` choisit le lecteur, et un exécutable `disc-launcher-data-data-audio` dans le PATH remplace tout.

## Cartouches : Retrode

La [Retrode](https://www.retrode.com/) se branche en USB et présente la cartouche comme un fichier ROM sur un volume `RETRODE`. Lister ce volume est instantané, mais **lire le fichier revient à dumper la cartouche** (lent). disc-launcher en tient compte :

1. **Identification sans dump.** Aucun octet du fichier n'est lu : sur la Retrode, lire ne serait-ce que le début du fichier déclenche le dump de toute la cartouche. disc-launcher se fie au nom du fichier, que la Retrode tire de l'en-tête (`Mariokart64.n64`, `MortalKombat.bin`), à son extension (système, d'après `RETRODE.CFG`) et à sa taille. Le titre est découpé (« Mariokart 64 », « Mortal Kombat ») puis cherché dans la base No-Intro (après `disc-launcher refdb fetch`) en ne comparant que lettres et chiffres : « Mario Kart 64 (Europe) ». Un titre tronqué par l'en-tête est accepté s'il ne désigne qu'un seul jeu ; à taille égale, la version d'origine européenne est préférée. Ce nom est provisoire : après le dump, l'empreinte SHA-1 donne le nom exact, et elle est mémorisée (`~/.cache/disc-launcher/carts.tsv`, clé : nom de fichier et taille) pour les insertions suivantes.
2. **Copie existante, quelle qu'en soit la version.** Les dossiers du système sont parcourus et un fichier dont le titre correspond (toutes révisions et régions confondues, ex. « Mario Kart 64 (USA) (Rev 1) ») est retenu. Le format préféré du manifeste passe en premier : pour la N64, un **WAD** de console virtuelle Wii (`formats.prefer = ["wad", "z64", …]`).
3. **Deux actions :**
   - **Jouer** : lance la copie existante ; sinon dumpe puis lance (jamais depuis la Retrode).
   - **Dumper / Re-dumper** : copie vers `~/ROMs/<système>/<nom No-Intro>.<ext>`, vérifiée par l'empreinte. Un WAD ou une autre version présents ne sont ni écrasés ni supprimés : la ROM est créée à côté.

Avec `[filenameChksum] 1` dans `RETRODE.CFG`, la Retrode ajoute une somme de contrôle au nom du fichier : deux révisions d'un même jeu ont alors des noms différents, ce qui fiabilise le cache des empreintes (la somme est retirée du titre si elle est hexadécimale et séparée par une espace, un tiret ou un souligné).

Les ROM N64 sont ramenées au format `.z64` quel que soit l'ordre d'octets produit (`.n64`, `.v64`) ; l'en-tête de copieur SNES est retiré. Systèmes : `n64`, `snes`, `megadrive`, `gb`, `gbc`, `gba`, `mastersystem`, `gamegear` (dossiers régionaux `sfc` et `genesis` comme ES-DE). Les extensions sont lues dans `RETRODE.CFG`. Les sauvegardes (`.srm`) ne sont pas copiées. `disc-launcher rom info <fichier>` analyse une ROM (en-tête, empreinte, nom) ; sur la Retrode, cela revient à la dumper.

L'émulateur par défaut est RetroArch : Mupen64Plus-Next pour les ROM N64, **Dolphin pour les WAD** (`existing_wad` du manifeste), Snes9x, Genesis Plus GX, Gambatte, mGBA. `[handlers.n64] emulator = "mupen64plus"` ou `program = "…"` en change ; un lanceur qui choisit lui-même l'émulateur (ES-DE…) se branche comme gestionnaire (`[handlers.n64] executable = { play = "…" }`).

## Gestionnaires

Chaque système (`psx`, `gc`…) et chaque média (`cdda`, `dvd-video`…) est servi par un exécutable. La première règle qui s'applique désigne celui-ci :

1. `[handlers.<id>] executable` de la configuration ;
2. l'exécutable déclaré par le manifeste ;
3. un exécutable `disc-launcher-<id>` (console) ou `disc-launcher-media-<id>` (média) présent dans le PATH ;
4. `[handlers.defaults]` de la configuration ;
5. le gestionnaire générique.

Une valeur peut viser un seul verbe, par exemple `{ play = "…", "*" = "disc-launcher-generic" }`. Il est ainsi possible de tout confier à RetroArch, qui choisit lui-même le cœur installé :

```toml
[handlers.defaults]
console = "disc-launcher-retroarch"
```

On peut aussi brancher un lanceur maison, qui lirait par exemple la configuration d'EmulationStation.

### Où sont les manifestes, et comment les modifier

Un manifeste (`psx.toml`, `media/cdda.toml`…) décrit un système : ses signatures, son plan de dump, ses émulateurs. Ils sont lus et **fusionnés** dans cet ordre, le dernier l'emportant clé par clé :

| Ordre | Emplacement | Rôle |
| --- | --- | --- |
| 1 | intégrés à l'exécutable | copie de `data/handlers/` au moment de la compilation |
| 2 | `/usr/share/disc-launcher/handlers/`, puis `/usr/local/share/disc-launcher/handlers/` | installés par `make install` (ou un paquet) ; remplacés à chaque installation |
| 3 | `~/.local/share/disc-launcher/handlers/` | vos ajouts et surcharges (et `make install-user`) |
| 4 | `~/.config/disc-launcher/handlers/` | idem, lu en dernier |

Ne modifiez pas les fichiers installés : une réinstallation les écrase. Pour un réglage simple, la configuration suffit ; pour le reste, écrivez dans `~/.local/share/disc-launcher/handlers/` un fichier du même nom ne contenant **que les clés à changer** :

```toml
# ~/.config/disc-launcher/config.toml : le plus simple
[handlers.psx]
program = "~/.local/bin/duckstation"   # AppImage ou nom différent

# ~/.local/share/disc-launcher/handlers/psx.toml : surcharge partielle du manifeste
[emulators.duckstation]
existing = ["duckstation", "-batch", "-fullscreen", "--", "{path}"]
```

Les programmes sont cherchés dans le PATH, puis dans `~/.local/bin`, `~/bin` et `~/Applications`, même si le démon a été lancé sans ces dossiers dans son PATH (symlinks vers des AppImages, scripts d'enveloppe…).

Pour un émulateur dont le programme porte plusieurs noms selon la distribution, le manifeste en donne une liste (`program = ["duckstation-qt", "duckstation", …]`) : le premier trouvé dans le PATH est retenu. `disc-launcher doctor` affiche le programme retenu entre parenthèses. Le démon, lancé par la session, voit le même PATH que vous si `~/.local/bin` y est ajouté par votre profil (cas d'Ubuntu) ; sinon, donnez un chemin complet avec `program`.

Chaque gestionnaire reçoit les informations déjà extraites en variables d'environnement `DL_*` : `DL_DEVICE`, `DL_LABEL`, `DL_SERIAL`, `DL_EXISTING`, `DL_INFO`, etc. Un script de quelques lignes suffit donc. Contrat complet et exemples (VLC, Lutris, RetroArch, lanceur PlayStation, lanceur ES-DE) : [docs/handlers.md](docs/handlers.md).

## Collection

L'index est une base SQLite : `~/.local/state/disc-launcher/collection.db`. Pour chaque fichier, il garde :

- le système et la clé d'identité du disque ;
- le **nom canonique prévu** et le **chemin réel** ;
- la **taille exacte**, une **empreinte partielle** (taille, premier et dernier Mio) et, quand il est connu, le **MD5** complet ;
- le SHA-1 du dump et son état de vérification.

Si vous renommez ou déplacez un fichier sous `~/ROMs`, disc-launcher le retrouve :

1. la taille désigne les candidats parmi les fichiers inconnus ;
2. l'empreinte partielle les départage, en ne lisant que 2 Mio par fichier ;
3. si l'entrée a un MD5 complet (c'est le cas de tout ce qui a été dumpé par disc-launcher), il confirme.

Le chemin réel est alors mis à jour et le nom canonique conservé. La recherche a lieu à l'insertion du disque correspondant, et lors de `disc-launcher collection scan`, qui parcourt tous les fichiers de `~/ROMs`. Le premier scan reste rapide : il ne calcule que les empreintes partielles, sauf avec `--full-hash`.

Les `.cue` ne sont pas indexés : ce sont de petits fichiers texte, trop semblables entre eux pour identifier un jeu. Un jeu cue/bin est indexé par sa première piste, et lancé par le `.cue` qui la référence.

```text
disc-launcher collection scan [--full-hash]  ajoute les fichiers inconnus, retrouve les renommages
disc-launcher collection list | prune [--days 30]
```

Après chaque dump, re-dump ou conversion réussis, la commande `[hooks] on_dumped` est exécutée, par exemple pour rafraîchir ES-DE ou sauvegarder la copie. `[handlers.<id>] on_dumped` la remplace pour un système. Elle reçoit le chemin produit (`{path}`, `DL_DUMPED_PATH`), le résultat de la vérification (`DL_VERIFIED`) et les variables `DL_*` habituelles.

## Premiers réglages utiles

```toml
# ~/.config/disc-launcher/config.toml
[general]
roms_dir = ""            # vide : réglage ROMDirectory d'ES-DE, sinon ~/ROMs
terminal = true          # suivre chaque dump dans un terminal (false : notifications seules)
# terminal_command = ["konsole", "--hold", "-e"]   # sinon : détection automatique

[drives."/dev/sr0"]
profile = "omnidrive"    # lecteur flashé (OmniDrive) ; voir disc-launcher doctor

[policy]
media = "auto-play"      # lire les médias sans question
psx = "dump-then-play"
```

`[policy]` remplace la question posée à l'insertion par une action automatique, système par système (`ask`, `auto-play`, `auto-dump`, `dump-then-play`, `ignore`).

**Suivre un dump.** Chaque dump ouvre un terminal qui affiche ses étapes et la sortie de redumper, chdman, etc. Le terminal est cherché dans cet ordre : `terminal_command`, `xdg-terminal-exec`, le terminal par défaut du bureau (KDE, Xfce, GNOME, Cinnamon, MATE), `$TERMINAL`, `x-terminal-emulator`, puis les terminaux courants. Fermer le terminal n'interrompt pas le dump ; `disc-launcher watch` le rouvre, et `disc-launcher cancel <id>` l'annule.

**Vrais noms des jeux.** Sans base de référence, disc-launcher ne connaît que ce que le disque contient (numéro de série, libellé) et donne un nom provisoire, comme `SLES-02905 (Europe)`. Pour obtenir `Rayman 2 - The Great Escape (Europe) (Fr,De)`, téléchargez les noms et numéros de série Redump publiés par libretro (une commande, quelques Mo par système) :

```sh
disc-launcher refdb fetch                  # tous les systèmes (ou : fetch psx ps2 saturn)
disc-launcher refdb                        # contenu de la base, par système
disc-launcher collection rename --dry-run  # fichiers déjà dumpés sous un nom provisoire
disc-launcher collection rename
```

Comment le nom est retrouvé, selon le système :

| Systèmes | Indice lu sur le disque | Correspondance |
| --- | --- | --- |
| PlayStation 1, 2, 3, Saturn, Mega-CD, Dreamcast, Xbox | numéro de série (`SLES-02905`, `T-4305G`, `EA-013`…) | numéro de série |
| GameCube, Wii | identifiant de jeu (`GALE01`) | code à 4 caractères du numéro (`DL-DOL-GALE-USA`) |
| PC Engine CD, PC-FX, PC-98, Neo Geo CD, 3DO, CD-i, CD32, CDTV, Jaguar CD | aucun numéro exploitable | taille de la première piste, si un seul jeu correspond ; sinon nom provisoire |
| Xbox 360, Wii U, FM Towns, jeux PC | — | DAT Redump complets seulement (tailles), ou nom provisoire |

Pour une prédiction exacte dès l'insertion (toutes les pistes) et une vérification complète, importez aussi les DAT officiels de Redump (redump.org, rubrique Downloads) ; les deux sources se complètent :

```sh
disc-launcher refdb import ~/Téléchargements/redump/*.dat
```

La base sert à trois choses :

- prédire le nom avant le dump : exact d'après les tailles des pistes (DAT Redump), probable d'après le numéro de série (libretro) ;
- vérifier le dump ensuite, par empreintes SHA-1, et renommer le fichier si besoin ;
- renommer après coup les fichiers au nom provisoire (`collection rename`).

**Systèmes sans correspondance par taille.** Pour GameCube et Wii, le résolveur `serials` lit `~/.local/share/disc-launcher/serials/<système>.txt`. Le format GameTDB `GALE01 = Titre` et le format TSV sont acceptés.

**Lecture brute.** L'assistant privilégié ne lance que les outils listés dans `/etc/disc-launcher/helper-tools.toml`. Vérifiez-y le chemin de `redumper` (par défaut `/usr/local/bin/redumper`, sinon `/usr/bin/redumper`).

## Utilisation

```text
disc-launcher status                     lecteurs, disques, cibles prédites, tâches
disc-launcher identify [/dev/srN]        identification + prédiction, sans le démon
disc-launcher run dump /dev/sr0          action sans notification (play-disc, play-existing…)
disc-launcher jobs | cancel <id>
disc-launcher watch [id]                 suivre une tâche en clair (la dernière par défaut)
disc-launcher log [-f] [--job <id>] [--level warn] [--json]
disc-launcher handlers [id]              exécutable retenu pour chaque verbe
disc-launcher reload                     relire la configuration
disc-launcher rom info <fichier>         cartouche : système, en-tête, empreinte, nom
disc-launcher refdb fetch [système…]     noms et numéros de série Redump (libretro) ; tous par défaut
disc-launcher refdb                      contenu de la base de référence
disc-launcher collection rename          nom canonique pour les fichiers au nom provisoire
disc-identify --image jeu.cue            classificateur seul (JSON)
```

## Vérifié, et à vérifier sur matériel

**Testé sans lecteur.** Les tests unitaires et `tests/e2e.py` couvrent :

- l'identification d'images synthétiques (PS1, PS2, Saturn, Mega-CD, GameCube, Wii, Xbox, Jaguar CD, PC Engine CD, VCD, CD audio, CD-Extra, Mixed Mode, partition vidéo Xbox, cue/bin multi-fichiers) ;
- la prédiction et la vérification de noms avec un DAT ;
- une tâche complète, avec des outils simulés, jusqu'au placement dans `Jeu.m3u/` ;
- l'index SQLite et la détection d'un renommage ;
- le démon ;
- les lecteurs multimédias et les variables `DL_*` ;
- le lecteur Kodi face à un faux serveur JSON-RPC ;
- la chaîne de résolution des exécutables et la détection de cœur RetroArch.

**À valider avec de vrais disques et de vrais outils :**

- les lignes de commande par défaut des outils de dump, des émulateurs et des lecteurs. Elles sont dans `data/handlers/` et `config.toml`, et se surchargent sans recompiler ;
- les signatures des systèmes les plus rares ;
- le comportement d'un lecteur OmniDrive face aux disques Nintendo ;
- les prédicats Solid sur votre version de Plasma.

## Arborescence du code

```text
src/sys.rs sqlite.rs        appels système ; liaison libsqlite3
src/json.rs toml.rs xml.rs  formats
src/dbus.rs                 client D-Bus (notifications, logind, udisks2)
src/device/                 lecteur réel (SG_IO), images .iso/.cue, source mémoire
src/fs/                     ISO 9660, XDVDFS, dossier monté
src/identify/               classificateur : TOC, signatures déclaratives, sondes, profils
src/naming.rs refdb.rs      nom canonique, cible ; base Redump (DAT)
src/collection.rs           index SQLite, renommages, existant, .m3u
src/handlers.rs             manifestes, résolution des exécutables, variables DL_*
src/cart.rs data.rs         cartouches (Retrode) ; disques de fichiers (musique, vidéo)
src/generic.rs media.rs retroarch.rs   gestionnaires génériques et RetroArch
src/jobs.rs daemon.rs       tâches détachées ; démon
src/bin/                    exécutables (dont disc-launcher-player-kodi)
data/                       manifestes (consoles, media/), signatures, configuration, .desktop, polkit
docs/handlers.md            contrat et exemples de gestionnaires
docs/dependances.md         installation des outils de dump (Ubuntu, Arch)
contrib/                    exemples de services systemd, OpenRC, runit, s6
```

## TODO

### Dépendances Rust (crates.io)

Le projet n'a aujourd'hui aucune dépendance Rust : crates.io n'était pas accessible pendant l'écriture. Les formats JSON, TOML et XML, le client D-Bus et la liaison SQLite sont donc écrits à la main (`src/json.rs`, `toml.rs`, `xml.rs`, `dbus.rs`, `sqlite.rs`). Ils sont testés et suffisent au projet, mais c'est du code à maintenir, et le lecteur TOML ne couvre qu'un sous-ensemble du format.

À faire : les remplacer par les bibliothèques de référence, `serde` + `serde_json`, `toml`, `quick-xml`, `zbus` et `rusqlite`. Chaque module a une interface étroite ; le remplacement peut se faire module par module. Contrepartie : un temps de compilation plus long et des dépendances à suivre.

### Lectures privilégiées sans mot de passe (polkit)

Les lectures brutes passent par `pkexec disc-launcher-helper`. La politique fournie (`data/polkit/io.github.flavi1.disclauncher.policy`) autorise l'utilisateur de la session active **sans mot de passe** (`allow_active = yes`), pour qu'un dump démarre sans interruption.

Conséquence : n'importe quel programme lancé dans votre session peut aussi appeler l'assistant. Le risque est limité, car l'assistant n'exécute que les outils listés dans `/etc/disc-launcher/helper-tools.toml`, sur un lecteur `/dev/srN`, avec des arguments filtrés, sous votre identité, et n'écrit que dans un dossier qui vous appartient. Mais c'est une porte vers des commandes SCSI constructeur ouverte sans confirmation.

Options à évaluer :

- `auth_admin_keep` : mot de passe demandé au premier dump, puis mémorisé quelques minutes ;
- une règle polkit (`/etc/polkit-1/rules.d/`) qui n'autorise sans mot de passe que les membres d'un groupe dédié, par exemple `disc-launcher` ;
- garder `yes` mais le documenter comme un choix de confort.

### Contrôle des arguments de l'assistant

L'assistant tourne avec un privilège (`CAP_SYS_RAWIO`) que l'utilisateur n'a pas d'ordinaire. Il ne doit donc pas exécuter n'importe quelle ligne de commande qu'on lui transmet : sinon, un programme malveillant pourrait lui faire lancer redumper avec des options dangereuses, ou écrire ailleurs que prévu.

Aujourd'hui, la tâche de dump construit la ligne de commande et l'assistant la vérifie **argument par argument**, contre la liste `allowed_args` de `helper-tools.toml` (`disc`, `--drive=`, `--image-path=`, `--speed=`…). C'est fragile dans les deux sens :

- quand redumper ajoute ou renomme une option utile, l'assistant la refuse (« argument non autorisé ») et le dump échoue, tant que la liste n'est pas mise à jour à la main ;
- une liste longue et permissive finit par laisser passer des combinaisons qu'on n'avait pas prévues.

À faire : remplacer cette liste par des **profils d'invocation complets**, définis uniquement par root dans `helper-tools.toml`, par exemple :

```toml
[profiles.redumper-disc]
command = ["/usr/bin/redumper", "disc", "--drive={sgdevice}", "--image-path={out}", "--image-name={name}"]
variables = { name = "nom-de-fichier", speed = "entier" }
```

La tâche ne transmettrait plus qu'un nom de profil et quelques valeurs typées (nom de fichier, vitesse). L'assistant construirait lui-même la commande. Rien d'autre ne pourrait être injecté, et une évolution de redumper ne demanderait que de modifier le profil.

### Lecture UDF interne

Pour identifier un disque sans le monter, disc-launcher lit lui-même son système de fichiers. Il sait lire l'**ISO 9660** (CD, et la plupart des DVD, qui ont aussi un système de fichiers « pont » ISO 9660) et le **XDVDFS** (Xbox).

Mais la plupart des Blu-ray vidéo, et certains DVD, n'ont qu'un système de fichiers **UDF** (version 2.50 pour les Blu-ray). Pour ces disques, disc-launcher doit passer par le point de montage : en général le bureau monte le disque tout seul, sinon disc-launcher demande le montage à udisks2. Si aucun montage n'est possible (pas de bureau, pas d'udisks2), le Blu-ray vidéo n'est pas reconnu et il est traité comme un disque de données.

À faire : écrire un lecteur UDF en lecture seule, y compris la « partition de métadonnées » propre à l'UDF 2.50, pour supprimer cette dépendance au montage. Il suffit de savoir lister un dossier et lire quelques petits fichiers (`BDMV/index.bdmv`, `PS3_GAME/PARAM.SFO`).

### Autres pistes

- **Validation sur matériel** : passer `disc-identify` sur de vrais disques de chaque famille, conserver les résultats comme jeux de tests, corriger les lignes de commande des outils et des émulateurs.
- **Disques Nintendo et OmniDrive** : vérifier si une lecture standard fonctionne avec le firmware OmniDrive ; sinon, implémenter la lecture brute de l'en-tête pour identifier le jeu avant le dump.
- **Tests de robustesse automatisés (fuzzing)** des lecteurs ISO 9660, XDVDFS, PARAM.SFO et cue, qui analysent des données venues du disque.
- **Intégration continue et paquets** : tests de bout en bout dans GitHub Actions, paquets AUR et Debian.
- **Résolveurs en ligne** facultatifs (GameTDB, bases de séries), désactivés par défaut.

## Licence

GPL-3.0-or-later, voir [LICENSE](LICENSE).

disc-launcher appelle des outils externes (redumper, chdman, émulateurs, lecteurs multimédias) comme des programmes séparés : leurs licences propres s'appliquent à eux. libsqlite3 est dans le domaine public.
