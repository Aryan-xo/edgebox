#!/bin/sh
# Some platforms (e.g. Fly.io) mount volumes owned by root. Fix ownership of
# the data directory, then drop to the unprivileged `edgebox` user.
set -e
if [ "$(id -u)" = 0 ]; then
    chown edgebox /data
    exec setpriv --reuid=edgebox --regid=edgebox --init-groups edgebox "$@"
fi
exec edgebox "$@"
