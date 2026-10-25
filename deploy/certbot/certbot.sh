#!/bin/sh
# Obtains the certificate for $DOMAIN on first start (HTTP-01 on port 80),
# then renews it twice a day, and copies it to /certs whenever it changes,
# readable by the relay's user (65534). The relay picks a new one up within
# a minute, without dropping anyone.
set -eu
: "${DOMAIN:?set DOMAIN to the host name of the relay}"

live="/etc/letsencrypt/live/$DOMAIN"
if [ ! -f "$live/fullchain.pem" ]; then
  if [ -n "${EMAIL:-}" ]; then account="--email $EMAIL"; else account="--register-unsafely-without-email"; fi
  # shellcheck disable=SC2086
  certbot certonly --standalone --non-interactive --agree-tos $account -d "$DOMAIN"
fi

copy() {
  # The key first: the relay retries until certificate and key match.
  for file in privkey.pem fullchain.pem; do
    if ! cmp -s "$live/$file" "/certs/$file"; then
      cp "$live/$file" "/certs/$file.new"
      chown 65534:65534 "/certs/$file.new"
      chmod 600 "/certs/$file.new"
      mv "/certs/$file.new" "/certs/$file"
      echo "copied $file"
    fi
  done
}

copy
while :; do
  sleep "${RENEW_EVERY:-43200}"
  certbot renew --quiet || echo "renewal failed; trying again in 12 hours"
  copy
done
