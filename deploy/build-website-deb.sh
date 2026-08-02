#!/bin/sh
# Builds radio-website_<version>_all.deb. Needs only dpkg-deb; run on the
# Debian PC (or any Debian container). The result lands in the repo root.
# The release workflow overrides the version with the git tag's.

set -eu

VERSION="${RADIO_WEBSITE_VERSION:-0.2.0-1}"

cd "$(dirname "$0")/.."
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir -p \
    "$stage/DEBIAN" \
    "$stage/var/www/radio" \
    "$stage/etc/lighttpd/conf-available" \
    "$stage/usr/share/radio-website"

cp -R website/public website/lib "$stage/var/www/radio/"
cp deploy/website/90-radio.conf "$stage/etc/lighttpd/conf-available/90-radio.conf"
cp deploy/website/zz-radio-pool.conf "$stage/usr/share/radio-website/zz-radio-pool.conf"

sed "s/@VERSION@/$VERSION/" deploy/website/control.in > "$stage/DEBIAN/control"
install -m 755 deploy/website/postinst "$stage/DEBIAN/postinst"
install -m 755 deploy/website/prerm "$stage/DEBIAN/prerm"

dpkg-deb --build --root-owner-group "$stage" "radio-website_${VERSION}_all.deb"
