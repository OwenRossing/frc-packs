#!/bin/sh
# Back up the FRC Packs database. Run nightly by frc-packs-backup.timer, and by install.sh before each update.
# Restore: see deploy/README.md.
set -eu
umask 077
dir="${BACKUP_DIR:-/var/backups/frc-packs}"
keep_days="${KEEP_DAYS:-14}"
file="$dir/frcpacks-$(date +%Y-%m-%d-%H%M%S).dump"
pg_dump --format=custom --file="$file.part" frcpacks
mv "$file.part" "$file"
find "$dir" -name 'frcpacks-*.dump' -mtime +"$keep_days" -delete
echo "Backed up to $file"
