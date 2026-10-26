#!/bin/sh
# certbot deploy hook: hands a new certificate to the relay, readable by its
# user. The relay notices the change and loads it within a minute, without
# dropping anyone. Install as /etc/letsencrypt/renewal-hooks/deploy/peerconnect.sh.
set -e
live=/etc/letsencrypt/live/${PEERCONNECT_DOMAIN:-$(basename "$RENEWED_LINEAGE")}
dir=/etc/peerconnect/tls
install -m 640 -o root -g peerconnect "$live/fullchain.pem" "$dir/fullchain.pem.new"
install -m 640 -o root -g peerconnect "$live/privkey.pem" "$dir/privkey.pem.new"
mv "$dir/privkey.pem.new" "$dir/privkey.pem"
mv "$dir/fullchain.pem.new" "$dir/fullchain.pem"
