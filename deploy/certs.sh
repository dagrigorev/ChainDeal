#!/usr/bin/env bash
# Creates a local, name-constrained development CA and a TLS certificate for
#   chaindeal.localhost, *.chaindeal.localhost, localhost, chaindeal.test, 127.0.0.1
#
# The CA can only ever sign localhost / *.localhost / *.test names (X.509 name
# constraints), so trusting it cannot be abused to impersonate real sites.
# Keys stay in deploy/certs/ (gitignored). Re-running reuses the existing CA.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p certs && chmod 700 certs && cd certs

if [[ ! -f ca.key ]]; then
  openssl ecparam -name prime256v1 -genkey -noout -out ca.key
  chmod 600 ca.key
  openssl req -x509 -new -key ca.key -sha256 -days 825 -out ca.crt \
    -subj "/O=ChainDeal (local development)/CN=ChainDeal Local Dev CA" \
    -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    -addext "nameConstraints=critical,permitted;DNS:localhost,permitted;DNS:.localhost,permitted;DNS:.test,permitted;IP:127.0.0.0/255.0.0.0"
  echo "created CA  deploy/certs/ca.crt"
fi

openssl ecparam -name prime256v1 -genkey -noout -out tls.key
chmod 600 tls.key
openssl req -new -key tls.key -out tls.csr -subj "/CN=chaindeal.localhost"
cat > tls.ext <<'EXT'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=serverAuth
subjectAltName=DNS:chaindeal.localhost,DNS:*.chaindeal.localhost,DNS:localhost,DNS:chaindeal.test,DNS:*.chaindeal.test,IP:127.0.0.1
EXT
# 397 days: within the lifetime browsers accept for server certificates.
openssl x509 -req -in tls.csr -CA ca.crt -CAkey ca.key -CAcreateserial -sha256 -days 397 -extfile tls.ext -out tls.crt
rm -f tls.csr tls.ext
openssl verify -CAfile ca.crt tls.crt
echo "issued      deploy/certs/tls.crt for chaindeal.localhost (valid 397 days)"
