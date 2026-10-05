# disc-launcher : compilation et installation. Dépendances : Rust, et libsqlite3
# (paquet de développement à la compilation, ex. libsqlite3-dev). Pas de systemd.
#
#   make                         compile (cargo build --release)
#   make test                    tests unitaires
#   sudo make install            installation système (PREFIX=/usr/local par défaut),
#                                après « make » : ne recompile pas
#   sudo make install-native     coquilles natives Solid/GIO (mode « hybride »)
#   make install-user            installation dans ~/.local, sans assistant privilégié
#   sudo make uninstall

PREFIX      ?= /usr/local
BINDIR      ?= $(PREFIX)/bin
LIBEXECDIR  ?= $(PREFIX)/libexec
DATADIR     ?= $(PREFIX)/share
SYSCONFDIR  ?= /etc
DESTDIR     ?=
CARGO       ?= cargo
# polkit ne lit ses actions et ses règles qu'à ces emplacements, quel que soit PREFIX.
POLKITACTIONSDIR ?= /usr/share/polkit-1/actions
POLKITRULESDIR   ?= /etc/polkit-1/rules.d
GROUP       ?= disc-launcher
TARGET      := target/release

BINS := disc-launcherd disc-launcher disc-identify disc-launcher-job disc-launcher-panel \
        disc-launcher-generic disc-launcher-media-generic disc-launcher-data-generic disc-launcher-retroarch \
        disc-launcher-player-kodi disc-launcher-resolve-serials

all: build

build:
	$(CARGO) build --release

test:
	$(CARGO) test

# install ne recompile pas : sous sudo, cargo (installé par rustup dans ~/.cargo)
# n'est généralement pas dans le PATH, et une compilation en root laisserait des
# fichiers appartenant à root dans target/. Lancer « make » d'abord.
built:
	@for b in $(BINS) disc-launcher-helper; do \
	  [ -x $(TARGET)/$$b ] || { echo "$(TARGET)/$$b absent : lancez d'abord « make » (sans sudo)." >&2; exit 1; }; \
	done

install: built
	install -d $(DESTDIR)$(BINDIR) $(DESTDIR)$(LIBEXECDIR)/disc-launcher
	for b in $(BINS); do install -m 755 $(TARGET)/$$b $(DESTDIR)$(BINDIR)/$$b; done
	install -m 755 $(TARGET)/disc-launcher-helper $(DESTDIR)$(LIBEXECDIR)/disc-launcher/disc-launcher-helper
	install -d $(DESTDIR)$(DATADIR)/disc-launcher/handlers/media $(DESTDIR)$(DATADIR)/disc-launcher/handlers/data $(DESTDIR)$(DATADIR)/disc-launcher/signatures
	install -m 644 data/handlers/*.toml $(DESTDIR)$(DATADIR)/disc-launcher/handlers/
	install -m 644 data/handlers/media/*.toml $(DESTDIR)$(DATADIR)/disc-launcher/handlers/media/
	install -m 644 data/handlers/data/*.toml $(DESTDIR)$(DATADIR)/disc-launcher/handlers/data/
	install -m 644 data/signatures/*.toml $(DESTDIR)$(DATADIR)/disc-launcher/signatures/
	install -m 644 data/drive-profiles.toml $(DESTDIR)$(DATADIR)/disc-launcher/
	install -d $(DESTDIR)$(SYSCONFDIR)/disc-launcher $(DESTDIR)$(SYSCONFDIR)/xdg/autostart
	[ -e $(DESTDIR)$(SYSCONFDIR)/disc-launcher/config.toml ] || install -m 644 data/config.toml $(DESTDIR)$(SYSCONFDIR)/disc-launcher/config.toml
	@f=$(DESTDIR)$(SYSCONFDIR)/disc-launcher/helper-tools.toml; \
	if [ ! -e $$f ]; then install -m 644 data/helper-tools.toml $$f; \
	elif ! cmp -s data/helper-tools.toml $$f; then install -m 644 data/helper-tools.toml $$f.dist; \
	  echo "NOTE : $$f conservé ; nouvelle version fournie dans $$f.dist (comparez avec : diff $$f $$f.dist)"; fi
	install -m 644 data/desktop/disc-launcherd-autostart.desktop $(DESTDIR)$(SYSCONFDIR)/xdg/autostart/disc-launcherd.desktop
	install -d $(DESTDIR)$(DATADIR)/applications $(DESTDIR)$(POLKITACTIONSDIR) $(DESTDIR)$(POLKITRULESDIR)
	install -m 644 data/desktop/disc-launcher.desktop $(DESTDIR)$(DATADIR)/applications/disc-launcher.desktop
	sed 's|@LIBEXECDIR@|$(LIBEXECDIR)|' data/polkit/io.github.flavi1.disclauncher.policy > $(DESTDIR)$(POLKITACTIONSDIR)/io.github.flavi1.disclauncher.policy
	sed 's|"disc-launcher"|"$(GROUP)"|' data/polkit/50-disc-launcher.rules > $(DESTDIR)$(POLKITRULESDIR)/50-disc-launcher.rules
	rm -f $(DESTDIR)$(DATADIR)/polkit-1/actions/io.github.flavi1.disclauncher.policy
	@# Groupe des utilisateurs autorisés sans mot de passe (pas lors d'un empaquetage).
	@if [ -z "$(DESTDIR)" ] && ! getent group $(GROUP) >/dev/null; then \
	  groupadd --system $(GROUP) && echo "Groupe $(GROUP) créé. Pour dumper sans mot de passe : sudo usermod -aG $(GROUP) \$$USER, puis reconnectez-vous."; \
	fi
	install -d $(DESTDIR)$(DATADIR)/doc/disc-launcher/contrib
	cd contrib && for f in $$(find . -type f); do \
	  d=$(DESTDIR)$(DATADIR)/doc/disc-launcher/contrib/$$(dirname $$f); install -d $$d; \
	  case $$f in *.in) o=$$d/$$(basename $$f .in); sed 's|@BINDIR@|$(BINDIR)|g' $$f > $$o; if [ -x $$f ]; then chmod 755 $$o; fi;; *) cp -p $$f $$d/;; esac; \
	done
	install -m 644 README.md LICENSE $(DESTDIR)$(DATADIR)/doc/disc-launcher/
	install -d $(DESTDIR)$(DATADIR)/doc/disc-launcher/docs
	install -m 644 docs/*.md $(DESTDIR)$(DATADIR)/doc/disc-launcher/docs/
	@echo "Un démon déjà lancé passe sur la nouvelle version tout seul, dès qu'il est au repos (aucun dump ni disque en attente)."

install-native:
	install -d $(DESTDIR)$(DATADIR)/solid/actions $(DESTDIR)$(DATADIR)/applications
	install -m 644 data/desktop/disc-launcher-play-solid.desktop $(DESTDIR)$(DATADIR)/solid/actions/disc-launcher-play.desktop
	install -m 644 data/desktop/disc-launcher-play-gio.desktop $(DESTDIR)$(DATADIR)/applications/disc-launcher-play.desktop
	-update-desktop-database $(DESTDIR)$(DATADIR)/applications 2>/dev/null

# Installation utilisateur (sans root) : pas d'assistant privilégié, donc pas de
# lecture OmniDrive/Kreon des commandes constructeur (voir README).
install-user: build
	install -d $(HOME)/.local/bin $(HOME)/.local/share/disc-launcher/handlers/media $(HOME)/.local/share/disc-launcher/handlers/data $(HOME)/.local/share/disc-launcher/signatures $(HOME)/.config/autostart
	for b in $(BINS); do install -m 755 $(TARGET)/$$b $(HOME)/.local/bin/$$b; done
	install -m 644 data/handlers/*.toml $(HOME)/.local/share/disc-launcher/handlers/
	install -m 644 data/handlers/media/*.toml $(HOME)/.local/share/disc-launcher/handlers/media/
	install -m 644 data/handlers/data/*.toml $(HOME)/.local/share/disc-launcher/handlers/data/
	install -m 644 data/signatures/*.toml $(HOME)/.local/share/disc-launcher/signatures/
	install -m 644 data/drive-profiles.toml $(HOME)/.local/share/disc-launcher/
	sed 's|^Exec=disc-launcherd|Exec=$(HOME)/.local/bin/disc-launcherd|' data/desktop/disc-launcherd-autostart.desktop > $(HOME)/.config/autostart/disc-launcherd.desktop

uninstall:
	for b in $(BINS); do rm -f $(DESTDIR)$(BINDIR)/$$b; done
	rm -rf $(DESTDIR)$(LIBEXECDIR)/disc-launcher $(DESTDIR)$(DATADIR)/disc-launcher $(DESTDIR)$(DATADIR)/doc/disc-launcher
	rm -f $(DESTDIR)$(SYSCONFDIR)/xdg/autostart/disc-launcherd.desktop
	rm -f $(DESTDIR)$(DATADIR)/applications/disc-launcher.desktop $(DESTDIR)$(DATADIR)/applications/disc-launcher-play.desktop
	rm -f $(DESTDIR)$(DATADIR)/solid/actions/disc-launcher-play.desktop
	rm -f $(DESTDIR)$(DATADIR)/polkit-1/actions/io.github.flavi1.disclauncher.policy
	rm -f $(DESTDIR)$(POLKITACTIONSDIR)/io.github.flavi1.disclauncher.policy $(DESTDIR)$(POLKITRULESDIR)/50-disc-launcher.rules
	@echo "Configuration conservée dans $(SYSCONFDIR)/disc-launcher ; groupe $(GROUP) conservé (sudo groupdel $(GROUP))"

.PHONY: all build built test install install-native install-user uninstall
