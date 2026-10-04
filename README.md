# oasis

firmware that turns an ESP32 into an offline community hotspot. join the
open `OASIS` wifi network and a captive portal opens with:

- **board**: a persistent, text only message board. topics (general,
  events, marketplace, lost & found, introductions) hold threads, and
  threads hold replies. an admin can pin threads
- **chat**: ephemeral chat, gone on reboot
- **mail**: private messages between visitors, with or without an account
- **files**: peer to peer file transfer between visitors over WebRTC. the
  device only introduces the two browsers, the file never passes through it

three buttons at the top right open notifications (new mail, replies to
your threads), a status page (who is around, how full the device is), and
your account. every name is a link to a profile with a "send mail" button.

there is no internet uplink. every device gets a generated name such as
`~amber-otter`. signing up replaces it with a username, protected by a
password, and adds an optional description. guests can do everything
accounts can, but a guest name and its mail belong to the device. the wifi is open
and unencrypted, so passwords can be read by anyone in radio range: do not
reuse one from elsewhere.

## hardware

a classic ESP32 with 4MB of flash (developed on an ESP32-D0WD-V3 dev board
with a CH340 usb serial adapter). about 2MB of flash holds the mail and message
board. the access point serves at most 10 clients at a time.

## building

the toolchain is pinned in `mise.toml`. with [mise](https://mise.jdx.dev)
installed:

    make setup      # once: installs rust, the xtensa rust fork, espflash, ...
    make            # builds the host binary and the firmware
    make flash      # writes the firmware to the board on /dev/ttyUSB0
    make monitor    # shows the serial log

the first firmware build downloads ESP-IDF into `firmware/.embuild`.

`make backup` saves the current flash contents of a board before it is
overwritten. use `PORT=/dev/ttyUSB1 make flash` for a different port.

### build time settings

| variable            | default | meaning                                  |
|---------------------|---------|------------------------------------------|
| `OASIS_SSID`        | `OASIS` | network name and portal title            |
| `OASIS_ADMIN_TOKEN` | unset   | enables posting news and deleting posts  |

    OASIS_SSID=camp OASIS_ADMIN_TOKEN=sesame make flash

## using

connect to the wifi network. most phones and laptops open the portal on
their own. otherwise browse to `http://oasis.local/` or `http://10.0.0.1/`.

`oasis.local` is announced over mDNS. it does not resolve on android 11 and
older, on linux without avahi or systemd-resolved mDNS, or through a VPN.
the address always works.

the small sign-in window that phones show for captive portals gets a
welcome page: what the oasis is, and how to open it in a regular browser
(the sign-in window often lacks WebRTC and downloads). a regular browser
goes straight to sign up on the first visit, which can be skipped.

on iphone the sign-in sheet shows "done" right away, and tapping it keeps
the phone on the wifi. the phone then believes this network has internet,
so it stops falling back to mobile data until it leaves.

to administer, open `http://10.0.0.1/#admin` and enter the admin token on
the board tab. threads then show pin and delete buttons, and replies a
delete button. up to 8 threads can be pinned per topic.

the network is open and unencrypted, so everything, including the admin
token, is visible to anyone in radio range.

## developing without hardware

    make run        # portal on http://127.0.0.1:8080/
    make test-e2e   # end-to-end tests against the host binary
    make precommit  # formatters and linters
