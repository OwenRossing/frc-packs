#!/usr/bin/env bash
# Install or update FRC Packs on an Ubuntu machine (22.04 or newer). Run it from a clone of the repo as your normal
# user; it asks for your password when it needs sudo. Running it again rebuilds from the current checkout, backs up
# the database and restarts the site.
#
#   git clone https://github.com/OwenRossing/frc-packs.git && cd frc-packs && ./deploy/install.sh
#
# It sets up:
#   - Postgres, with a database "frcpacks" that only the "frcpacks" system user can reach (no password to keep)
#   - the server and site in /opt/frc-packs, run by systemd as "frc-packs" on 127.0.0.1:3000 (this machine only)
#   - a nightly database backup to /var/backups/frc-packs, kept for two weeks
#   - the admin account, the first time (its password is printed at the end), and the `frc-packs` admin command
# Putting it on your domain is a separate step with Cloudflare Tunnel: see deploy/README.md.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
prefix=/opt/frc-packs
svc_user=frcpacks
db=frcpacks

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
# version_ge A B: true when version A >= version B.
version_ge() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" = "$2" ]; }
# Run as the postgres superuser, from a directory it can read.
pg() { (cd / && sudo -u postgres "$@"); }

command -v sudo >/dev/null || { echo "This script needs sudo." >&2; exit 1; }
command -v apt-get >/dev/null || { echo "This script is for Ubuntu (or Debian)." >&2; exit 1; }

step "Installing system packages"
sudo apt-get update -q
sudo apt-get install -y -q postgresql build-essential pkg-config curl ca-certificates git rsync gnupg

step "Checking Rust"
need_rust="$(sed -n 's/^rust-version *= *"\(.*\)"/\1/p' "$repo/server/Cargo.toml")"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "$HOME/.cargo/env"
fi
have_rust="$(rustc --version | awk '{print $2}')"
if ! version_ge "$have_rust" "$need_rust"; then
  if command -v rustup >/dev/null; then
    rustup update stable && rustup default stable
  else
    echo "Rust $have_rust is too old (need $need_rust). Remove Ubuntu's rustc and cargo packages" >&2
    echo "(sudo apt-get remove rustc cargo) and run this again; it will install a current Rust." >&2
    exit 1
  fi
fi
rustc --version

step "Checking Node"
need_node=20.19.0 # what Vite needs
have_node="$(node --version 2>/dev/null | tr -d v || true)"
if [ -z "$have_node" ] || ! version_ge "$have_node" "$need_node"; then
  echo "Installing Node 22 from NodeSource (found: ${have_node:-none})"
  sudo install -d -m 755 /etc/apt/keyrings
  curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key |
    sudo gpg --dearmor --yes -o /etc/apt/keyrings/nodesource.gpg
  echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_22.x nodistro main" |
    sudo tee /etc/apt/sources.list.d/nodesource.list >/dev/null
  sudo apt-get update -q
  sudo apt-get install -y -q nodejs
fi
node --version

step "Building the server (the first build takes a while)"
(cd "$repo/server" && cargo build --release --locked)

step "Building the site"
(cd "$repo/web" && PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci --no-audit --no-fund && npm run build)

step "Setting up the database"
sudo systemctl enable --now postgresql
id "$svc_user" >/dev/null 2>&1 ||
  sudo useradd --system --user-group --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin "$svc_user"
[ -n "$(pg psql -tAc "select 1 from pg_roles where rolname = '$svc_user'")" ] || pg createuser "$svc_user"
[ -n "$(pg psql -tAc "select 1 from pg_database where datname = '$db'")" ] || pg createdb -O "$svc_user" "$db"

step "Installing to $prefix"
sudo install -d -m 755 "$prefix" "$prefix/bin" "$prefix/data" "$prefix/web"
sudo install -d -m 700 -o "$svc_user" -g "$svc_user" /var/backups/frc-packs
sudo install -m 755 "$repo/deploy/backup.sh" "$prefix/bin/frc-packs-backup"
sudo install -m 755 "$repo/deploy/frc-packs" /usr/local/bin/frc-packs
sudo install -m 644 "$repo"/deploy/frc-packs{,-backup}.service "$repo/deploy/frc-packs-backup.timer" /etc/systemd/system/
sudo systemctl daemon-reload
if systemctl is-active --quiet frc-packs; then
  echo "Backing up the database before updating"
  sudo systemctl start frc-packs-backup.service
fi
sudo install -m 755 "$repo/server/target/release/frc-packs-server" "$prefix/bin/frc-packs-server"
copy() { sudo rsync -rlt --delete --chmod=D755,F644 "$1" "$2"; }
copy "$repo/data/packs/" "$prefix/data/packs/"
copy "$repo/data/photos/" "$prefix/data/photos/"
copy "$repo/web/dist/" "$prefix/web/"

step "Starting the site"
sudo systemctl enable --quiet frc-packs.service frc-packs-backup.timer
sudo systemctl restart frc-packs.service
sudo systemctl start frc-packs-backup.timer
bind="$( (systemctl show -p Environment --value frc-packs || true) | tr ' ' '\n' | sed -n 's/^BIND=//p' | tail -1)"
bind="${bind:-127.0.0.1:3000}"
for _ in $(seq 1 30); do
  if curl -fsS "http://$bind/api/health" >/dev/null 2>&1; then
    printf '\n\033[1mFRC Packs is running at http://%s\033[0m (reachable from this machine only).\n' "$bind"
    echo "Next: point a Cloudflare Tunnel at http://$bind. See deploy/README.md."
    echo "Logs: journalctl -u frc-packs -f"
    echo
    sudo frc-packs create-admin
    exit 0
  fi
  sleep 1
done
echo "The site didn't come up. Its last log lines:" >&2
sudo journalctl -u frc-packs -n 30 --no-pager >&2
exit 1
