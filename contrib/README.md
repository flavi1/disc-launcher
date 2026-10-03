# Exemples de services (utilisateurs avertis)

La seule méthode de démarrage prise en charge est **XDG Autostart**
(`/etc/xdg/autostart/disc-launcherd.desktop`, voir le README). Les fichiers de
ce dossier sont des exemples, fournis tels quels, pour qui veut faire superviser
le démon par son gestionnaire de services. Les copies installées dans
`$(PREFIX)/share/doc/disc-launcher/contrib/` ont déjà le bon chemin des
exécutables à la place de `@BINDIR@`.

Avant d'en utiliser un, sachez que :

- **le démon est un service utilisateur**, jamais root : il utilise votre bus
  D-Bus de session, votre affichage et `~/ROMs` ;
- **il lui faut l'environnement graphique** (`DISPLAY` ou `WAYLAND_DISPLAY`,
  `DBUS_SESSION_BUS_ADDRESS`, `XDG_RUNTIME_DIR`) : c'est à vous de le lui
  fournir, ce que XDG Autostart fait sans rien demander ;
- **désactivez l'entrée XDG Autostart** pour votre compte, sinon deux démons
  démarrent (le second s'arrête aussitôt, mais vous ne savez pas lequel tourne) :

  ```sh
  mkdir -p ~/.config/autostart
  printf '[Desktop Entry]\nType=Application\nName=disc-launcher\nHidden=true\n' \
    > ~/.config/autostart/disc-launcherd.desktop
  ```

  Supprimez ce fichier pour revenir à XDG Autostart ;
- **n'arrêtez que le démon, pas son groupe de processus** : les dumps en cours
  et les programmes lancés en sont détachés et doivent survivre à un arrêt.

## systemd

```sh
mkdir -p ~/.config/systemd/user
cp /usr/local/share/doc/disc-launcher/contrib/systemd/disc-launcherd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now disc-launcherd
```

Le service suit `graphical-session.target`, atteinte sous KDE Plasma et GNOME,
qui fournissent aussi l'environnement graphique au gestionnaire utilisateur.
Journal : `journalctl --user -u disc-launcherd` ou `disc-launcher log`.

## OpenRC (≥ 0.60, services utilisateur)

```sh
sudo cp /usr/local/share/doc/disc-launcher/contrib/openrc/disc-launcherd /etc/user/init.d/
rc-update --user add disc-launcherd default
rc-service --user disc-launcherd start
```

Les services utilisateur OpenRC démarrent à la connexion, souvent avant le
bureau : vérifiez que l'environnement graphique leur parvient
(`disc-launcher doctor`, et le journal du démon). Avant OpenRC 0.60, il n'y a
pas de services utilisateur : restez sur XDG Autostart.

## runit, s6

Copiez `runit/run` ou `s6/run` comme script `run` d'un service surveillé par
un `runsvdir` ou un `s6-svscan` lancé **sous votre compte**, et exportez-y
l'environnement graphique. Pour s6-rc, ajoutez un fichier `type` contenant
`longrun`.
