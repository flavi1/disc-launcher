#!/usr/bin/env python3
"""Tests de bout en bout sans lecteur ni matériel.

Simule redumper et chdman par des scripts, puis vérifie :
  1. le plan de dump rendu par un gestionnaire générique ;
  2. l'import d'un DAT et la vérification après dump (renommage canonique) ;
  3. l'exécution complète d'une tâche (disc-launcher-job) : progression,
     placement dans ~/ROMs/<système>/Jeu.m3u/, liste .m3u, index SQLite ;
  4. la détection d'un renommage par l'utilisateur (taille + MD5) ;
  5. le démarrage du démon sans D-Bus, le socket de contrôle, l'arrêt propre ;
  6. le gestionnaire multimédia générique : lecteurs configurables, variables
     DL_*, lecteur Kodi (faux serveur JSON-RPC) ;
  7. la chaîne de résolution des exécutables et le gestionnaire RetroArch.

Usage : python3 tests/e2e.py [target/debug|target/release]
"""
import hashlib, json, os, re, shutil, signal, sqlite3, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "target/debug"))
ROOT = tempfile.mkdtemp(prefix="dl-e2e-")
HOME = os.path.join(ROOT, "home")
RUN = os.path.join(ROOT, "run")
FAKE = os.path.join(ROOT, "fakebin")
ROMS = os.path.join(ROOT, "ROMs")
for d in (HOME, RUN, FAKE, ROMS):
    os.makedirs(d, mode=0o700, exist_ok=True)

ENV = dict(os.environ)
ENV.update(HOME=HOME, XDG_RUNTIME_DIR=RUN, DISC_LAUNCHER_DATA_DIR=os.path.join(HERE, "data"),
           DISC_LAUNCHER_SYSCONF_DIR=os.path.join(ROOT, "etc"), PATH=f"{FAKE}:{BIN}:{os.environ['PATH']}",
           LANG="fr_FR.UTF-8", DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent", DBUS_SYSTEM_BUS_ADDRESS="unix:path=/nonexistent")
for k in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
    ENV.pop(k, None)

FAKEDATA = b"FAKE-PSX-TRACK-1" * 1000

def write(path, text, mode=0o644):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, mode)

write(os.path.join(FAKE, "redumper"), f"""#!/usr/bin/env python3
import sys, time
args = dict(a.split('=', 1) for a in sys.argv[2:] if '=' in a)
p, n = args['--image-path'], args['--image-name']
for i in range(0, 101, 25):
    print(f"[{{i:3d}}%] LBA: {{i*100}}", flush=True)
    time.sleep(0.05)
open(f"{{p}}/{{n}} (Track 1).bin", "wb").write({FAKEDATA!r})
open(f"{{p}}/{{n}}.cue", "w").write(f'FILE "{{n}} (Track 1).bin" BINARY\\n  TRACK 01 MODE2/2352\\n    INDEX 01 00:00:00\\n')
open(f"{{p}}/{{n}}.log", "w").write("log")
""", 0o755)
write(os.path.join(FAKE, "chdman"), """#!/bin/sh
# chdman createcd --force -i in.cue -o out.chd
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift;; -i) in="$2"; shift;; esac; shift; done
echo "Compressing, 50.0% complete... (ratio=40.0%)"
echo "CHD of $in" > "$out"
echo "Compression complete ... final ratio = 40.0%"
""", 0o755)
write(os.path.join(HOME, ".config/disc-launcher/config.toml"), f"""[general]
roms_dir = "{ROMS}"
helper = "none"
eject_after_read = false
terminal = false

[hooks]
on_dumped = ["sh", "-c", "echo \\"$DL_DUMPED_PATH|$DL_VERIFIED|{{system}}\\" > {ROOT}/hook.out"]
""")

def run(*cmd, inp=None, check=True):
    r = subprocess.run(cmd, input=inp, stdin=None if inp is not None else subprocess.DEVNULL, capture_output=True, text=True, env=ENV, timeout=60)
    if check and r.returncode != 0:
        raise SystemExit(f"échec {cmd}: {r.returncode}\n{r.stdout}\n{r.stderr}")
    return r

ok = 0
def check(cond, label):
    global ok
    if not cond:
        raise SystemExit(f"✗ {label}")
    ok += 1
    print(f"✓ {label}")

# 1. Plan de dump
inp = json.dumps({"physical": {"media": "cd"}, "profile": "standard", "device": "/dev/sr9", "tmp": "/T", "stem": "Game", "identity": {"system": "psx"}})
plan = json.loads(run("disc-launcher-generic", "--id", "psx", "dump-plan", inp=inp).stdout)
check(plan["steps"][0]["command"] == ["redumper", "disc", "--drive=/dev/sr9", "--image-path=/T", "--image-name=Game"], "plan : commande de lecture rendue")
check(plan["outputs"] == ["Game.chd"] and plan["verify"] == ["Game*.bin"], "plan : sorties et fichiers vérifiés")
r = run("disc-launcher-generic", "--id", "gc", "dump-plan", inp=json.dumps({"physical": {"media": "dvd"}, "profile": "standard"}), check=False)
check(r.returncode == 2 and "incompatible" in r.stdout, "plan : GameCube refusé sur lecteur standard (code 2)")

# 1b. Plans de repli : sans redumper, cdrdao pour les CD et dd pour les DVD.
FB = os.path.join(ROOT, "fallback-bin")
for t in ["cdrdao", "toc2cue", "chdman"]:
    write(os.path.join(FB, t), "#!/bin/sh\nexit 0\n", 0o755)
ENV_FB = dict(ENV, PATH=f"{FB}:{BIN}:/usr/bin:/bin")
def plan_fb(sysid, media):
    r = subprocess.run(["disc-launcher-generic", "--id", sysid, "dump-plan"], env=ENV_FB, capture_output=True, text=True,
                       input=json.dumps({"physical": {"media": media}, "profile": "standard", "device": "/dev/sr9", "tmp": "/T", "stem": "Game"}))
    return json.loads(r.stdout)
p = plan_fb("psx", "cd")
check(p["plan"] == "cdrdao" and p["fallback"] is True and p["exact"] is False and p["verify"] == [] and p["steps"][0]["command"][0] == "cdrdao", "repli : cdrdao pour un CD sans redumper")
p = plan_fb("ps2", "dvd")
check(p["plan"] == "dd" and p["exact"] is True and p["steps"][0]["command"][:2] == ["dd", "if=/dev/sr9"] and p["outputs"] == ["Game.chd"], "repli : dd pour un DVD sans redumper")
out = subprocess.run(["disc-launcher", "doctor"], env=ENV_FB, capture_output=True, text=True).stdout
line = next(l for l in out.splitlines() if l.strip().startswith("psx "))
check("✓" in line and "repli : cd : cdrdao" in line, "doctor : plan de repli signalé")

# 2. Base de référence
sha = hashlib.sha1(FAKEDATA).hexdigest()
dat = os.path.join(ROOT, "psx.dat")
write(dat, f"""<?xml version="1.0"?><datafile><header><name>Sony - PlayStation</name></header>
<game name="Fake Quest (Europe) (Disc 2)"><rom name="Fake Quest (Europe) (Disc 2).cue" size="1" sha1="00"/>
<rom name="Fake Quest (Europe) (Disc 2).bin" size="{len(FAKEDATA)}" sha1="{sha}"/></game>
<game name="Fake Quest (Europe) (Disc 1)"><rom name="Fake Quest (Europe) (Disc 1).bin" size="5" sha1="11"/></game></datafile>""")
out = run("disc-launcher", "refdb", "import", dat).stdout
check("2 jeux importés" in out, "refdb : import du DAT")

# 3. Tâche complète
state_dir = os.path.join(HOME, ".local/state/disc-launcher")
stem = "Fake Quest [SCES-99999] (Europe) (Disc 2)"
sysdir = os.path.join(ROMS, "psx")
tmp = os.path.join(sysdir, ".disc-launcher-tmp", "t1")
target = {"system_dir": sysdir, "folder": "psx", "path": os.path.join(sysdir, stem + ".chd"), "stem": stem, "ext": "chd", "game_dir": None, "m3u": None}
inp = json.dumps({"physical": {"media": "cd"}, "profile": "standard", "device": "/dev/null", "tmp": tmp, "stem": stem})
steps = json.loads(run("disc-launcher-generic", "--id", "psx", "dump-plan", inp=inp).stdout)
job_id = "20260101-000000-test"
jd = os.path.join(state_dir, "jobs", job_id)
os.makedirs(jd)
planv = {"id": job_id, "kind": "dump", "device": None, "system": "psx", "title": stem, "key": "psx:SCES-99999:d2:Europe",
         "identity": {"system": "psx", "serial": "SCES-99999", "disc": 2, "discs_total": 2, "region": "Europe"},
         "resolution": {"name": stem, "game": "Fake Quest [SCES-99999] (Europe)", "disc": 2, "discs": 2, "confidence": "repli", "source": "fallback"},
         "target": target, "tmp": tmp, "stem": stem, "steps": steps["steps"], "outputs": steps["outputs"], "verify": steps["verify"],
         "estimated_bytes": 1000, "options": {"eject_after_read": False}}
json.dump(planv, open(os.path.join(jd, "plan.json"), "w"))
open(os.path.join(jd, "lock"), "w").close()
json.dump({"id": job_id, "status": "pending", "created": int(time.time())}, open(os.path.join(jd, "state.json"), "w"))
run("disc-launcher-job", job_id)
st = json.load(open(os.path.join(jd, "state.json")))
check(st["status"] == "done" and st["progress"] == 100, f"tâche terminée ({st.get('message')})")
w = run("disc-launcher", "watch", job_id).stdout
check("[" in w and "commande" in w and "Terminé" in w and "[100%]" in w, "watch : étapes, sortie des outils et résultat")
final = st["result"]["path"]
check(final == os.path.join(sysdir, "Fake Quest (Europe).m3u", "Fake Quest (Europe) (Disc 2).chd"), "renommage canonique après vérification + dossier .m3u")
check(open(final).read().startswith("CHD of"), "fichier CHD placé")
check(st["result"]["verified"] == "ok", "dump conforme à la base")
m3u = os.path.join(sysdir, "Fake Quest (Europe).m3u", "Fake Quest (Europe).m3u")
check(open(m3u).read() == "Fake Quest (Europe) (Disc 2).chd\n", "liste .m3u écrite")
check(not os.path.exists(tmp), "dossier temporaire supprimé")
check(open(f"{ROOT}/hook.out").read().strip() == f"{final}|ok|psx", "crochet on_dumped : variables et gabarits")
db = sqlite3.connect(os.path.join(state_dir, "collection.db"))
row = db.execute("SELECT key, verified, canonical_name, size, md5, path, quick_hash FROM entries WHERE system = 'psx'").fetchone()
check(row[6] is not None, "index SQLite : empreinte partielle")
check(row[0] == "psx:SCES-99999:d2:Europe" and row[1] == "ok", "index SQLite : clé et vérification")
check(row[2] == "Fake Quest (Europe) (Disc 2)" and row[3] == os.path.getsize(final), "index SQLite : nom canonique et taille exacte")
check(row[4] == hashlib.md5(open(final, "rb").read()).hexdigest(), "index SQLite : MD5")
db.close()
log = open(os.path.join(jd, "job.log")).read()
check("level=info comp=job" in log and "progress=" in log, "journal de tâche au format logfmt")

# 3b. Cartouche N64 sur une Retrode (volume simulé) : identification, copie normalisée, nom No-Intro
retro = os.path.join(ROOT, "RETRODE")
os.makedirs(retro)
write(os.path.join(retro, "RETRODE.CFG"), "[n64RomExt] n64 ; ext\n")
z64 = bytearray(0x2000)
z64[0:4] = bytes([0x80, 0x37, 0x12, 0x40]); z64[0x20:0x2B] = b"MARIOKART64"; z64[0x3B:0x3F] = b"NKTP"
n64 = bytearray(z64)
for i in range(0, len(n64), 4):
    n64[i:i+4] = n64[i:i+4][::-1]
open(os.path.join(retro, "Mariokart64.n64"), "wb").write(n64)
zsha = hashlib.sha1(z64).hexdigest().upper()
write(os.path.join(ROOT, "n64.dat"), f'clrmamepro ( name "Nintendo - Nintendo 64" )\ngame ( name "Mario Kart 64 (Europe)" serial "NKTP" rom ( name "Mario Kart 64 (Europe).z64" size {len(z64)} sha1 {zsha} serial "NKTP" ) )\n')
run("disc-launcher", "refdb", "import", os.path.join(ROOT, "n64.dat"))
info = run("disc-launcher", "rom", "info", os.path.join(retro, "Mariokart64.n64")).stdout
check("n64" in info and "NKTP" in info and "Mario Kart 64 (Europe)" in info, "cartouche : système, code et nom No-Intro (ordre d'octets .n64 normalisé)")
csys = os.path.join(ROMS, "n64")
ctmp = os.path.join(csys, ".disc-launcher-tmp", "c1")
cinp = json.dumps({"physical": {"media": "cart"}, "profile": "standard", "device": os.path.join(retro, "Mariokart64.n64"), "tmp": ctmp, "stem": "MARIOKART64 [NKTP] (Europe)"})
csteps = json.loads(run("disc-launcher-generic", "--id", "n64", "dump-plan", inp=cinp).stdout)
check(csteps["steps"][0]["command"][:3] == ["disc-launcher", "rom", "copy"], "cartouche : plan de copie")
cid = "20260101-000001-cart"
cjd = os.path.join(state_dir, "jobs", cid)
os.makedirs(cjd)
ctarget = {"system_dir": csys, "folder": "n64", "path": os.path.join(csys, "MARIOKART64 [NKTP] (Europe).z64"), "stem": "MARIOKART64 [NKTP] (Europe)", "ext": "z64", "game_dir": None, "m3u": None}
json.dump({"id": cid, "kind": "dump", "device": os.path.join(retro, "Mariokart64.n64"), "system": "n64", "title": "MARIOKART64", "key": "n64:NKTP:d1:Europe", "cart": True,
           "identity": {"system": "n64", "game_id": "NKTP", "region": "Europe", "disc": 1},
           "resolution": {"name": "MARIOKART64 [NKTP] (Europe)", "confidence": "repli", "source": "fallback"},
           "target": ctarget, "tmp": ctmp, "stem": ctarget["stem"], "steps": csteps["steps"], "outputs": csteps["outputs"], "verify": csteps["verify"],
           "estimated_bytes": len(z64), "options": {"eject_after_read": False}}, open(os.path.join(cjd, "plan.json"), "w"))
open(os.path.join(cjd, "lock"), "w").close()
json.dump({"id": cid, "status": "pending", "created": int(time.time())}, open(os.path.join(cjd, "state.json"), "w"))
run("disc-launcher-job", cid)
cst = json.load(open(os.path.join(cjd, "state.json")))
check(cst["status"] == "done" and cst["result"]["verified"] == "ok" and cst["result"]["path"] == os.path.join(csys, "Mario Kart 64 (Europe).z64"), f"cartouche : copie vérifiée et renommée ({cst.get('message')})")
check(open(cst["result"]["path"], "rb").read() == bytes(z64), "cartouche : copie au format z64")

# 4. Renommage par l'utilisateur, retrouvé au scan
renamed = os.path.join(sysdir, "Mes jeux", "FQ2.chd")
os.makedirs(os.path.dirname(renamed))
os.rename(final, renamed)
out = run("disc-launcher", "collection", "scan").stdout
check("renommé" in out and "FQ2.chd" in out, "scan : renommage retrouvé (taille + MD5)")
db = sqlite3.connect(os.path.join(state_dir, "collection.db"))
row = db.execute("SELECT path, canonical_name, missing_since FROM entries WHERE key = 'psx:SCES-99999:d2:Europe'").fetchone()
check(row[0] == renamed and row[1] == "Fake Quest (Europe) (Disc 2)" and row[2] is None, "scan : chemin réel mis à jour, nom canonique conservé")
db.close()

# 5. Démon
write(os.path.join(FAKE, "retroarch"), "#!/bin/sh\nexit 0\n", 0o755)
d = subprocess.Popen([os.path.join(BIN, "disc-launcherd"), "--foreground"], env=dict(ENV, DISC_LAUNCHER_RETRODE_DIRS=retro), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
for _ in range(50):
    if os.path.exists(os.path.join(RUN, "disc-launcher", "control.sock")):
        break
    time.sleep(0.1)
st = json.loads(run("disc-launcher", "status", "--json").stdout)
check(st["ok"] is True, "démon : socket de contrôle")
rom_path = os.path.join(retro, "Mariokart64.n64")
seen = None
for _ in range(60):
    st = json.loads(run("disc-launcher", "status", "--json").stdout)
    drv = [x for x in st.get("drives", []) if x.get("device") == rom_path]
    if drv and drv[0].get("offer"):
        seen = drv[0]
        break
    time.sleep(0.2)
check(seen is not None and "console:n64" in json.dumps(seen["identification"]) and "Mario Kart 64 (Europe)" in json.dumps(seen["offer"]), "démon : cartouche Retrode détectée, identifiée, proposée")
check(seen["offer"]["actions"] == ["play-existing", "redump", "open-files"] and seen["offer"]["situation"]["path"].endswith("Mario Kart 64 (Europe).z64"), "démon : cartouche déjà dumpée → Jouer la copie / Re-dumper / Ouvrir")
# Version WAD (console virtuelle Wii) d'une autre région : prioritaire pour « Jouer »
wad = os.path.join(csys, "Mario Kart 64 (USA) (Virtual Console).wad")
open(wad, "wb").write(b"WAD")
run("disc-launcher", "refresh", rom_path, check=False)
seen = None
for _ in range(60):
    st = json.loads(run("disc-launcher", "status", "--json").stdout)
    drv = [x for x in st.get("drives", []) if x.get("device") == rom_path]
    if drv and (drv[0].get("offer") or {}).get("situation", {}).get("path", "").endswith(".wad"):
        seen = drv[0]
        break
    time.sleep(0.2)
check(seen is not None, "démon : version WAD retrouvée par le titre et préférée")
os.rename(rom_path, rom_path + ".bak")
gone = False
for _ in range(40):
    st = json.loads(run("disc-launcher", "status", "--json").stdout)
    if not any(x.get("device") == rom_path for x in st.get("drives", [])):
        gone = True
        break
    time.sleep(0.2)
check(gone, "démon : cartouche retirée")
os.rename(rom_path + ".bak", rom_path)
run("disc-launcher", "reload")
d2 = subprocess.run([os.path.join(BIN, "disc-launcherd")], env=ENV, capture_output=True, text=True, timeout=10)
check(d2.returncode == 0, "démon : instance unique")
d.send_signal(signal.SIGTERM)
d.wait(timeout=10)

# 5b. Fenêtre « Disques et périphériques » (protocole de disc-launcher-panel,
# ici un faux panneau) et icône StatusNotifierItem sur un bus de session privé,
# avec une clé USB simulée et un faux gestionnaire de fichiers.
if shutil.which("dbus-daemon") and shutil.which("gdbus"):
    bus = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address=1"], stdout=subprocess.PIPE, text=True)
    addr = bus.stdout.readline().strip()
    usbdir = os.path.join(ROOT, "usb")
    os.makedirs(usbdir)
    states = os.path.join(ROOT, "panel-states")
    write(os.path.join(FAKE, "fm"), f"#!/bin/sh\necho \"$@\" > {ROOT}/fm-opened\n", 0o755)
    write(os.path.join(FAKE, "fake-panel"), f"""#!/usr/bin/env python3
import json, sys
print("ready", flush=True)
clicked = False
with open({states!r}, "a") as log:
    for line in sys.stdin:
        log.write(line); log.flush()
        st = json.loads(line)
        usb = [i for i in st["items"] if i["id"].startswith("usb:")]
        if usb and st["visible"] and not clicked:
            clicked = True
            print("action\\t" + usb[0]["id"] + "\\topen-files", flush=True)
""", 0o755)
    write(os.path.join(HOME, ".config/disc-launcher/config.toml"), f'[general]\nfile_manager = ["{FAKE}/fm"]\n[ui]\npanel_command = ["{FAKE}/fake-panel"]\n'
          f'[actions.marque]\nlabel = "Marquer"\nhandlers = ["usb"]\ncommand = "echo \\"$DL_HANDLER $DL_LABEL\\" > {ROOT}/custom-ran"\n')
    tenv = dict(ENV, DBUS_SESSION_BUS_ADDRESS=addr, DISC_LAUNCHER_USB_DIRS=f"MA_CLE={usbdir}")
    d = subprocess.Popen([os.path.join(BIN, "disc-launcherd"), "--foreground"], env=tenv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    def gd(*a):
        return subprocess.run(["gdbus", "call", "--session", *a], env=tenv, capture_output=True, text=True).stdout
    def last_state():
        try:
            lines = open(states).read().splitlines()
            return json.loads(lines[-1]) if lines else None
        except FileNotFoundError:
            return None
    st = None
    for _ in range(50):
        st = last_state()
        if st and any(i["id"].startswith("usb:") for i in st["items"]):
            break
        time.sleep(0.2)
    usb = [i for i in (st or {}).get("items", []) if i["id"].startswith("usb:")]
    check(bool(usb) and usb[0]["title"].startswith("Volume USB — MA_CLE") and usb[0]["usage"] and "libres sur" in usb[0]["usage"]["text"], "fenêtre : volume USB avec barre d'espace libre")
    check(st is not None and st["visible"] is False, "fenêtre : volume présent au démarrage → pas d'ouverture spontanée")
    name = None
    for _ in range(50):
        m = re.search(r"org\.kde\.StatusNotifierItem-\d+-1", gd("--dest", "org.freedesktop.DBus", "--object-path", "/", "--method", "org.freedesktop.DBus.ListNames"))
        if m:
            name = m.group(0)
            break
        time.sleep(0.2)
    props = gd("--dest", name or "x", "--object-path", "/StatusNotifierItem", "--method", "org.freedesktop.DBus.Properties.GetAll", "org.kde.StatusNotifierItem")
    check("'IconName': <'media-eject'>" in props and "'Status': <'Active'>" in props and "'ItemIsMenu': <false>" in props, "icône : StatusNotifierItem « media-eject », active, sans menu")
    gd("--dest", name or "x", "--object-path", "/StatusNotifierItem", "--method", "org.kde.StatusNotifierItem.Activate", "--", "0", "0")
    for _ in range(30):
        if os.path.exists(os.path.join(ROOT, "fm-opened")):
            break
        time.sleep(0.1)
    check(os.path.exists(os.path.join(ROOT, "fm-opened")) and open(os.path.join(ROOT, "fm-opened")).read().strip() == usbdir, "clic sur l'icône → fenêtre affichée ; « Ouvrir » → gestionnaire de fichiers")
    time.sleep(0.8)
    check(last_state()["visible"] is False, "fenêtre : refermée après une action")
    gd("--dest", name or "x", "--object-path", "/StatusNotifierItem", "--method", "org.kde.StatusNotifierItem.ContextMenu", "--", "0", "0")
    time.sleep(0.8)
    check(last_state()["visible"] is True, "clic droit sur l'icône → fenêtre réaffichée")
    usb = [i for i in last_state()["items"] if i["id"].startswith("usb:")]
    check(bool(usb) and ["custom:marque", "Marquer"] in usb[0]["actions"], "action personnalisée ([actions.marque]) proposée pour le volume USB")
    subprocess.run([os.path.join(BIN, "disc-launcher"), "run", "custom:marque", usb[0]["id"] if usb else "x"], env=tenv, capture_output=True)
    for _ in range(30):
        if os.path.exists(os.path.join(ROOT, "custom-ran")):
            break
        time.sleep(0.1)
    check(os.path.exists(os.path.join(ROOT, "custom-ran")) and open(os.path.join(ROOT, "custom-ran")).read().strip() == "usb MA_CLE", "action personnalisée exécutée avec les variables DL_*")
    d.send_signal(signal.SIGTERM)
    d.wait(timeout=10)
    bus.terminate()
    os.remove(os.path.join(HOME, ".config/disc-launcher/config.toml"))

dlog = open(os.path.join(state_dir, "log", "daemon.log")).read()
check("msg=démarrage" in dlog and "msg=arrêt" in dlog and "msg=capabilities" in dlog, "démon : journal interne (démarrage, capacités, arrêt)")

# 6. Gestionnaire multimédia générique
write(os.path.join(FAKE, "fakeplayer"), f"""#!/bin/sh
{{ echo "ARGS=$*"; env | grep '^DL_' | sort; }} > {ROOT}/player.out
""", 0o755)
with open(os.path.join(HOME, ".config/disc-launcher/config.toml"), "a") as f:
    f.write('\n[media]\nplayer = "auto"\nauto_order = ["absent", "fake"]\n'
            '[media.players.absent]\ncommand = ["/nonexistent/player"]\n'
            '[media.players.fake]\nname = "Faux lecteur"\ndvd-video = ["fakeplayer", "dvd://{device}", "{label}"]\n')
r = run("disc-launcher-media-generic", "play", "--disc", "/dev/nonexistent", check=False)
check(r.returncode != 0 and "error" in r.stdout, "média : erreur propre sans lecteur")
envp = dict(ENV, DL_CHECKED="1", DL_MEDIA="dvd-video", DL_DEVICE="/dev/sr9", DL_LABEL="MON_FILM", DL_TAG="video:dvd")
r = subprocess.run(["disc-launcher-media-generic", "describe", "--id", "dvd-video"], env=envp, capture_output=True, text=True, stdin=subprocess.DEVNULL)
check(json.loads(r.stdout)["player_name"] == "Faux lecteur", "média : lecteur choisi (repli auto_order)")
r = subprocess.run(["disc-launcher-media-generic", "play", "--id", "dvd-video"], env=envp, capture_output=True, text=True, stdin=subprocess.DEVNULL)
check(r.returncode == 0, "média : lecture lancée")
for _ in range(50):
    if os.path.exists(f"{ROOT}/player.out"):
        break
    time.sleep(0.1)
time.sleep(0.2)
po = open(f"{ROOT}/player.out").read()
check("ARGS=dvd:///dev/sr9 MON_FILM" in po, "média : gabarits {device} et {label}")
check("DL_MEDIA=dvd-video" in po and "DL_TAG=video:dvd" in po, "média : variables DL_* transmises au lecteur")

# Lecteur Kodi (optionnel) face à un faux serveur JSON-RPC
import socket, threading
srv = socket.socket(); srv.bind(("127.0.0.1", 0)); srv.listen(5)
port = srv.getsockname()[1]
calls = []
def serve():
    while True:
        try:
            c, _ = srv.accept()
        except OSError:
            return
        req = json.loads(c.recv(65536).decode())
        calls.append(req)
        res = "pong" if req["method"] == "JSONRPC.Ping" else "OK"
        c.sendall(json.dumps({"jsonrpc": "2.0", "method": "Player.OnPlay", "params": {}}).encode() + json.dumps({"id": 1, "jsonrpc": "2.0", "result": res}).encode())
        c.close()
threading.Thread(target=serve, daemon=True).start()
with open(os.path.join(HOME, ".config/disc-launcher/config.toml"), "a") as f:
    f.write(f'\n[media.players.kodi]\ntcp_port = {port}\nhttp_port = 1\naddon_params = ["device={{device}}"]\n')
r = subprocess.run(["disc-launcher-player-kodi"], env=dict(ENV, DL_DEVICE="/dev/sr0"), capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=30)
check("json-rpc" in r.stdout, "lecteur Kodi : addon déclenché par JSON-RPC")
ex = [c for c in calls if c["method"] == "Addons.ExecuteAddon"]
check(ex and ex[0]["params"] == {"addonid": "script.disc.import", "params": ["device=/dev/sr0"], "wait": False}, "lecteur Kodi : Addons.ExecuteAddon(script.disc.import, device=/dev/sr0)")
srv.close()

# Pas de blocage si un lanceur laisse l'entrée standard ouverte sans écrire
p = subprocess.Popen(["disc-launcher-media-generic", "describe", "--id", "cdda"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=ENV, text=True)
try:
    p.wait(timeout=5)
    check(p.returncode == 0, "entrée standard ouverte : pas de blocage")
finally:
    p.kill()

# 7. Chaîne de résolution et RetroArch
out = run("disc-launcher", "handlers", "psx").stdout
check("play: disc-launcher-generic" in out, "résolution : générique console par défaut")
write(os.path.join(FAKE, "disc-launcher-media-cdda"), "#!/bin/sh\necho '{}'\n", 0o755)
out = run("disc-launcher", "handlers", "cdda").stdout
check("play: disc-launcher-media-cdda" in out, "résolution : exécutable spécifique du PATH prioritaire")
cores = os.path.join(ROOT, "cores")
os.makedirs(cores)
open(os.path.join(cores, "swanstation_libretro.so"), "w").close()
write(os.path.join(cores, "swanstation_libretro.info"), 'supported_extensions = "cue|chd|pbp"\ndatabase = "Sony - PlayStation"\n')
write(os.path.join(FAKE, "retroarch"), f"#!/bin/sh\necho \"$*\" > {ROOT}/ra.out\n", 0o755)
with open(os.path.join(HOME, ".config/disc-launcher/config.toml"), "a") as f:
    f.write(f'\n[handlers.defaults]\nconsole = {{ play = "disc-launcher-retroarch", "*" = "disc-launcher-generic" }}\n[retroarch]\ncores_dir = "{cores}"\n')
out = run("disc-launcher", "handlers", "psx").stdout
check("play: disc-launcher-retroarch" in out and "dump-plan: disc-launcher-generic" in out, "résolution : table par verbe dans [handlers.defaults]")
r = subprocess.run(["disc-launcher-retroarch", "play"], env=dict(ENV, DL_SYSTEM="psx", DL_EXISTING=renamed), capture_output=True, text=True, stdin=subprocess.DEVNULL)
check(r.returncode == 0, "RetroArch : lancement")
for _ in range(50):
    if os.path.exists(f"{ROOT}/ra.out"):
        break
    time.sleep(0.1)
time.sleep(0.2)
check(open(f"{ROOT}/ra.out").read().strip() == f"-L {cores}/swanstation_libretro.so {renamed}", "RetroArch : cœur détecté (swanstation)")
d = json.loads(subprocess.run(["disc-launcher-retroarch", "describe"], env=dict(ENV, DL_SYSTEM="psx"), capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout)
check(d["actions"]["play-existing"] is True and d["emulator"] == "retroarch/swanstation", "RetroArch : describe")

out = run("disc-launcher", "doctor").stdout
check("gestionnaires" in out and "psx" in out, "doctor")

shutil.rmtree(ROOT) if not os.environ.get("KEEP") else print(ROOT)
print(f"\n{ok} vérifications réussies")
