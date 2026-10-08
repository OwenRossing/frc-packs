# Hosting FRC Packs

This puts the site on your own domain from a Linux machine through Cloudflare Tunnel. Nothing on your network is opened to the internet: the machine connects out to Cloudflare, and Cloudflare serves the site over HTTPS. The same steps work on a laptop at home now and on a school server later.

You need:

- An Ubuntu machine (22.04 or newer) that stays on and online. 2 GB of RAM is plenty to run it; the first build is the slowest part.
- A domain on Cloudflare (the free plan is fine).

## 1. Install the site

```sh
git clone https://github.com/OwenRossing/frc-packs.git
cd frc-packs
./deploy/install.sh
```

Run it as your normal user; it asks for your password when it needs sudo. It installs whatever is missing (Postgres, Rust, Node), builds the server and the site, and starts it as a service on `http://127.0.0.1:3000`. That address only works on the machine itself, which is the point: the tunnel is the only way in.

Check it with `curl http://127.0.0.1:3000/api/health`. It should print `{"ok":true}`.

The first time, it also makes the **admin account** and prints its username (`admin`) and a generated password at the end. Save that password, then change it in Settings after you sign in. Running the script again doesn't make another one.

## 2. Keep a laptop awake

Skip this on a server. On a laptop, this stops it from sleeping when the lid closes or it sits idle:

```sh
sudo mkdir -p /etc/systemd/logind.conf.d
printf '[Login]\nHandleLidSwitch=ignore\nHandleLidSwitchExternalPower=ignore\nHandleLidSwitchDocked=ignore\n' |
  sudo tee /etc/systemd/logind.conf.d/frc-packs.conf
sudo systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target
sudo reboot
```

Keep it plugged in. Ethernet is steadier than Wi-Fi if you can reach the router.

## 3. Put it on your domain

In the Cloudflare dashboard:

1. Open **Tunnels** (under Networking, or Zero Trust → Networks, depending on your dashboard) and create a tunnel. Pick **Cloudflared** and name it `frc-packs`.
2. Choose **Debian**, **64-bit**, copy the install command it shows, and run it on the machine. It installs `cloudflared` as a service, so the tunnel comes back after a reboot. Wait for the dashboard to show the tunnel as connected.
3. Add a route of type **Published application** (older dashboards call it **Public hostname**): subdomain `packs` (or whatever you like), your domain, service type **HTTP**, URL `localhost:3000`. Save. Cloudflare adds the DNS record itself.
4. Open `https://packs.<your domain>`.

**Pick a hostname you'll keep.** Invite links point at it, and people's browsers stay signed in to it. Moving to another machine is fine (see below).

## 4. Invite people

Sign in on the site as `admin`, open **Settings → Admin → Open panel** (or go to `/admin`), and make invite codes. Each code can be for one person or for a group (set **People per code**), with a note so you remember who it's for. Send people the code or the link; the link opens the sign-up form with the code filled in. They pick a username and password once, and after that they sign in with those on any device.

From the same panel you can see every account, give packs, reset someone's password (the panel shows the new one once), turn an account off (it's signed out and can't sign in, but keeps its cards) or delete it.

Lost the admin password? On the machine:

```sh
sudo frc-packs reset-password admin
```

### Keep it to your team (optional)

While you test in house, Cloudflare can ask for an email code before anyone sees the site. In Zero Trust, add an **Access application** of type **Self-hosted** for the same hostname, with an **Allow** policy that includes your team's emails (or every email ending in your school's domain). It's free for up to 50 people.

## Updating

```sh
cd frc-packs
git pull
./deploy/install.sh
```

It backs up the database, rebuilds, and restarts. A pack someone was halfway through revealing picks up where it left off.

## Everyday commands

| Command | What it does |
| --- | --- |
| `systemctl status frc-packs` | Is it running? |
| `journalctl -u frc-packs -f` | Live logs |
| `sudo systemctl restart frc-packs` | Restart it |
| `sudo systemctl edit frc-packs` | Change a setting (the top of `deploy/frc-packs.service` shows how) |
| `sudo frc-packs reset-password <name>` | Give an account a new password and print it |

Never set `DEV_TOOLS=1` on a hosted site: it lets anyone give themselves packs and Mythic luck.

## Backups

Every night at 4am (or at the next start, if the machine was off), the database is saved to `/var/backups/frc-packs`, and two weeks of backups are kept. To back up right now: `sudo systemctl start frc-packs-backup`.

Those backups are on the same disk as the site, so copy one somewhere else now and then:

```sh
sudo ls /var/backups/frc-packs
sudo cp /var/backups/frc-packs/frcpacks-<date>.dump ~/
sudo chown "$USER" ~/frcpacks-<date>.dump
```

To restore one:

```sh
sudo systemctl stop frc-packs
sudo -u frcpacks pg_restore --clean --if-exists -d frcpacks < frcpacks-<date>.dump
sudo systemctl start frc-packs
```

## Moving to the school server

Do it in this order. If the tunnel reaches both machines at once, players can land on the one with the empty database and lose their account.

1. **School server:** run step 1. The site runs there locally, with no tunnel yet.
2. **Laptop:** take the site offline and save the latest cards.
   ```sh
   sudo cloudflared service uninstall
   sudo systemctl stop frc-packs
   sudo systemctl start frc-packs-backup
   sudo ls /var/backups/frc-packs
   sudo cp /var/backups/frc-packs/frcpacks-<newest>.dump ~/
   sudo chown "$USER" ~/frcpacks-<newest>.dump
   ```
3. Copy that file to the school server (USB stick, `scp`, anything).
4. **School server:** restore it (see Backups), then run the same tunnel's install command from the dashboard (open the `frc-packs` tunnel to see it again).

Same tunnel, same hostname: everyone keeps their accounts and cards, and nothing changes for players. For the school's IT: the machine only makes outbound connections (to Cloudflare on port 443, and 7844 if allowed); nothing inbound needs opening.
