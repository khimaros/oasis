#!/bin/sh
# runs drive.mjs against a fresh portal and a headless chrome.
#
# the portal listens on the LAN address instead of loopback: WebRTC needs
# the address that the portal sees a client under to be one the other
# client can reach, as it is on the device.
set -eu

HTTP_PORT=18080
DNS_PORT=15353
DEBUG_PORT=9333
STARTUP_SECS=2
ADMIN_USER=admin
ADMIN_PASSWORD=sesame

address=$(ip -4 route get 1.1.1.1 | grep -oP 'src \K[0-9.]+')
work=$(mktemp -d)
# chrome keeps writing its profile while it shuts down, so wait for it before
# removing the directory, and do not let a leftover file fail the run
trap 'kill $portal $chrome 2>/dev/null; wait 2>/dev/null; rm -rf "$work" || true' EXIT

# the settings file of the device, with the admin that drive.mjs logs in as
printf '[user]\nname = %s\npassword = %s\nadmin = yes\n' "$ADMIN_USER" "$ADMIN_PASSWORD" >"$work/oasis.conf"
OASIS_HTTP_ADDR="$address:$HTTP_PORT" OASIS_DNS_ADDR="127.0.0.1:$DNS_PORT" \
	OASIS_DATA_DIR="$work/data" OASIS_CHAT_INTERVAL_MS=0 OASIS_BOARD_INTERVAL_MS=0 \
	OASIS_CONFIG="$work/oasis.conf" \
	target/debug/oasis-host >"$work/portal.log" 2>&1 &
portal=$!
google-chrome --headless=new --disable-gpu --no-first-run --user-data-dir="$work/chrome" \
	--remote-debugging-port="$DEBUG_PORT" about:blank >"$work/chrome.log" 2>&1 &
chrome=$!
sleep "$STARTUP_SECS"

node tests/browser/drive.mjs "$DEBUG_PORT" "http://$address:$HTTP_PORT/" "$ADMIN_USER" "$ADMIN_PASSWORD"
