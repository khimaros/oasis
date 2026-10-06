# requirements

oasis is firmware for an ESP32 that offers an offline community portal
over its own wifi hotspot.

## hardware

- R1: target the classic ESP32 (ESP32-D0WD-V3, 4MB flash, no PSRAM assumed)
- R2: single app slot, no OTA updates, to maximize user data storage
- R37: the same firmware builds for the ESP32-S3, with 4, 8, or 16MB of
  flash. the board grows with the flash. PSRAM is not assumed
- R30: a second target, the raspberry pi 4 model B, with the same
  functionality. oasis is the only program on the device: a minimal linux
  kernel boots straight into it
- R31: on the raspberry pi the data lives on the sd card, and the limits on
  what is stored grow with the card

## network

- R3: open wifi access point, no upstream internet
- R4: captive portal: joining the network leads the client to the portal
  without typing an address

- R17: the portal is reachable as `oasis.local` (mDNS) and by its address.
  visitors in a captive sign-in window are told both, so they can move to
  their regular browser

## portal

- R5: there is no news section. an admin can pin threads in any topic, and
  pinned threads lead that topic's list
- R6: text only message board, persistent across reboots, oldest content
  evicted once storage is full. three levels with a back button: a vertical
  list of topics, each with a short description (general, events,
  marketplace, lost & found, introductions), the threads of a topic, and
  the messages of a thread. a topic shows how many threads it holds, and a
  thread in the list how many replies it has: the bare number in a small
  pill at the right end of the card's first line, and nothing at zero
- R7: ephemeral chat, held in RAM only, lost on reboot
- R8: every device gets a generated default name, derived from a hash of
  its mac address. a visitor can sign up with a unique username, a password,
  and an optional description, and log in from any device. username,
  password, and description can be changed later
- R33: admins are accounts with an admin bit, and log in like anyone. they
  pin threads, delete threads and replies, and make other accounts admins
  from their profile page, or end that. the account button of an admin has
  another color
- R35: pin and delete are small icon buttons on the first line of a thread
  card or a reply, left of the count. only admins have them for now. the
  pin of a pinned thread is drawn in the accent color. delete asks first,
  in the dialog of R32 with a cancel button next to delete
- R36: the visitor's own entry in the list of who is around, and their own
  profile page, are marked as theirs
- R34: a small settings file is flashed along with the program. it names
  the network, lists accounts that the device makes sure exist, admins
  among them, and threads that a fresh board starts with
- R18: everyone can send and receive private mail, guests under their
  generated name. an account is not needed
- R21: empty lists are shown empty, without placeholder text
- R24: a status page, opened by a button left of the account button. it
  shows who is around, with different dot colors for people who have the
  oasis open and devices that are only on the wifi, how many of each kind
  of object are stored out of how many fit, and how much space is used on
  each partition
- R25: a notification button for incoming mail and for replies to threads
  the visitor takes part in
- R27: one notification per mail and per reply, each on two lines (what
  happened, and how the message starts) with a small icon for its kind.
  mail notifications say who the mail is from
- R28: writing happens behind an action button: the forms for a new mail,
  a new thread, and a reply stay closed until it, or a reply button, opens
  them. the reply button on a mail is a filled button. an open form is a
  panel docked above the tab bar, with a title, a cross at its top right
  that cancels, and a send icon at its bottom right, where the action
  button was. chat has the same panel, always open, with the same icon
- R29: a lost connection to the device is shown by the status button
  turning red, not by text in the page
- R32: what the device refuses is shown in a small dialog with an okay
  button, not in a line of text that stays
- R26: every name shown is a link to that person's profile page, which has
  a button to send them mail
- R23: navigation goes through the address (url hash), so that the
  browser's back button and the arrow in the header behave the same and
  predictably. sign up and log in are a view of the main page with the same
  back arrow, not an overlay
- R22: the look, as chosen from mockups by the product owner: a thin
  pastel gradient band along the top instead of a title, cards on a dark
  background, a colored dot per topic, a floating tab bar, a sky blue
  accent (`#8fc3ee`), and one small corner radius for everything. the back
  arrow and the account button are equal squares at the two ends of the
  header, the account one outlined in the accent color with a person icon
  for guests and accounts alike. onboarding pages share one structure: a
  small mark, a heading with a lead line, cards, and a single main button
- R19: the navigation sits at the bottom of the screen. it and the chat box
  stay visible while the on-screen keyboard is open
- R20: an onboarding flow. the captive sign-in window shows a single page:
  what the oasis is and instructions for opening it in a regular browser
  (including bookmark or home screen). on iOS the system "done" is available
  from the moment the page loads, so that closing the sheet does not
  disconnect the phone. the page has no done button of its own. android
  gets a link that leads out of the sign-in window into the real browser.
  a regular browser is told, after the sign up step, to bookmark the page
  or add it to the home screen. a regular
  browser never shows that
  page: a first visit goes straight to sign up, and after that to the main
  page with the account button in the top right
- R9: usable without javascript frameworks or external assets, since
  clients have no internet access

## footprint

- R15: served files are as compact and simple as possible and a page load
  takes as few requests as possible. no images beyond a simple svg or favicon
- R16: the device avoids heavy processing. parsing, filtering, and rendering
  are pushed to the clients

## peer to peer

- R13: services are peer to peer wherever possible. clients share a subnet
  and connect to each other directly over WebRTC. the device only brokers
  presence and signaling
- R14: peer to peer file sharing between connected clients, on the `files`
  tab. file contents never pass through the device

## engineering

- R10: written in rust
- R11: build toolchain pinned and installed through mise wherever possible
- R12: portal logic runs unmodified on a development host so that it can be
  tested end to end without hardware
