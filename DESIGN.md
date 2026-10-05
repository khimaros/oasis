# design

## layout

    crates/portal   platform independent portal, std only, zero dependencies
    crates/host     runs the portal on a development host
    crates/rpi      raspberry pi 4 binary: init of a linux image, then the portal
    firmware        ESP32 binary: wifi access point + littlefs, then the portal
    tools           builds the sd card image of the raspberry pi
    tests/e2e       python tests that drive the host binary over sockets
    tests/browser   a headless chrome that clicks through the page
    tests/rpi       boots the raspberry pi image in qemu

`firmware` is excluded from the cargo workspace because it builds with the
xtensa rust fork (`firmware/rust-toolchain.toml`) and `build-std`.

the firmware is std rust on ESP-IDF (`esp-idf-svc`). ESP-IDF provides
sockets and a filesystem behind `std::net` and `std::fs`, which is what lets
the portal crate run unchanged on both targets (R12).

## network

the device runs an open access point on 10.0.0.1/24. its dhcp server names
10.0.0.1 as the dns server.

captive portal detection relies on two parts:

- `dns.rs` answers every A query with the portal address
- `routes.rs` redirects any request whose `Host` header is not the portal
  to `http://10.0.0.1/`, which catches the connectivity probes of every OS

the firmware also announces `oasis.local` through the ESP-IDF mDNS
component. clients with Private DNS or DNS over HTTPS never ask our dns, so
names only it knows fail for them, while `.local` is resolved by multicast.
`Config.aliases` lists such names: they are served without a redirect and
sent to clients in the poll response.

linux has neither a dhcp server nor an mdns responder in the kernel, so the
raspberry pi runs the ones of the portal crate:

- `dhcp.rs` leases the addresses .2 to .254 for two hours, with the portal
  as router and dns server. leases live in RAM. a client that comes back
  after a reboot asks for its old address and gets it unless it is taken.
  replies to clients without an address are broadcast
- `mdns.rs` answers A queries for `oasis.local` by multicast, or directly
  when the query came from a port other than 5353

captive sign-in windows are embedded web views that often lack WebRTC and
downloads. `index.html` recognizes them by user agent, a heuristic that
runs on the client, and steers people to a regular browser (see onboarding).

the page cannot be an installable PWA: service workers and install prompts
need https, and the device cannot hold a certificate that browsers trust
for a local name or address. meta tags let a home screen shortcut open
without browser chrome where the OS allows it.

## http

`http.rs` is a small http/1.1 server on `std::net::TcpListener`: a fixed
pool of worker threads, one request per connection, bounded head and body
sizes. the fixed pool bounds memory on the device. bodies are urlencoded
forms and responses are hand written json, which avoids a json dependency.

browsers open connections ahead of time and leave them unused. a worker
that waited on such a connection would be lost to every other client for
the length of the read timeout. so `serve` in `lib.rs` accepts connections
itself, keeps up to six that have sent nothing, and hands each to a worker
once its request starts to arrive. std has no `poll`, so it looks at them
every 10 ms. a connection that stays silent for five seconds is closed, and
so is the oldest one when a seventh arrives.

the UI is a single `index.html` with inline css and js, embedded in the
binary, so a page load is one request.

clients call `GET /api/poll` every two seconds. that one request returns
new chat messages, the peer list, pending signals, and the thread count of
each topic.

## storage

the board is a set of topics. the topic ids live in `lib.rs`, their labels
and descriptions in `index.html`.

`board.rs` keeps two logs per topic: threads (a quarter of the topic's
storage) and replies. the reference of a thread record is the id its first
reply would get, so readers know where its replies start. the reference of
a reply is the thread's id. the first line of a thread's text is its
subject.

- the thread list is one segment of the thread log, sent as stored
- a thread's replies are found by scanning the reply log from the thread's
  marker and keeping the records of that thread. this is the one place where
  the device filters. it sends at most 16KB per request and the id to
  continue after
- a page of threads comes with the number of replies of each thread, a u16
  per record. `Store::count` reads the reply log for that, from where the
  replies of the oldest thread on the page start, or only the segments
  that the index names. nothing is kept in RAM, so the cost is flash reads
  on a request that happens when someone opens a topic
- the number of threads of each topic is known without reading anything
  and goes out with the poll, for the topic list
- pinned thread ids (at most 8 per topic) are kept in a `pinned` file and
  sent with every thread page. the client fetches pinned threads that live
  on older pages by asking for the page that holds them
- only an admin can pin, or delete threads and replies
- threads and replies are evicted separately, oldest first. pinning does
  not protect a thread from eviction

`store.rs` keeps a log as numbered segment files of at most 8000 bytes,
named by a prefix and the id of their first record.

- a record is binary, little endian: `[length u16][ts u32][reference u32]
  [name length u8][name][text]`, 11 bytes around the name and the text. the
  length counts what follows it. the reference is free for the owner of
  the log
- a record's id is its position: the first id of its segment plus its
  index. ids are therefore not stored
- appending writes one record to the newest segment
- a full log evicts by deleting the oldest segment file
- a page of the UI is one segment, sent as stored, so reads never scan
- only the list of segment ids lives in RAM
- a record torn by power loss is dropped at the next boot
- deletions are ids appended to a `deleted` file
- a log too large to scan keeps an index (`Config.index_replies`, for the
  reply logs): which segments hold records of each reference. it is a map
  in RAM and an `index` file of `[reference u32][segment u32]` entries. an
  entry is written ahead of the first record it stands for, so power loss
  leaves at worst an entry that points at a segment without such a record.
  entries of evicted segments are dropped at boot, and a missing file is
  rebuilt from the records. the ESP32 goes without: its reply log is 22
  segments, and the map has no fixed bound

a littlefs directory costs two blocks of 4096 bytes, and a file larger
than about 500 bytes costs whole blocks. so logs do not get a directory
each. all board logs share `topics/`, named `<topic>.t.<id>` (threads),
`<topic>.r.<id>` (replies), and `<topic>.pinned`. all mailboxes share
`mail/`, named `<owner>.<id>`.

8000 bytes fills two blocks. the 2496KB data partition is 624 blocks, and
the worst case is counted in blocks:

| what                                             | blocks | KB   |
|--------------------------------------------------|--------|------|
| board: 5 topics x (7 thread + 22 reply segments) | 292    | 1196 |
| mail: 100 mailboxes x 2 segments                 | 202    | 827  |
| accounts, salt, secret, root                     | 16     | 66   |
| headroom for copy on write                       | 114    | 467  |

the board's text budget is 1200KB, split evenly between the five topics
(60KB of threads and 180KB of replies each). every limit is enforced by
eviction, so the worst case cannot be exceeded.

## identity

`users.rs` identifies a device by a hash of its mac address, salted with a
random value created on first boot so that ids cannot be traced back to a
mac. the firmware finds the mac through the dhcp lease of the client's
address (`Config.mac_of`). on a host every source address is its own device.

a client that is not logged in posts under a generated name, `~` plus two
words picked by its device id. usernames may not start with `~`. clients
cannot choose a name per request, the device decides it.

an account is a unique username (compared ignoring case), a password, and
an optional description that others see in its profile.

- passwords are stored as pbkdf2-hmac-sha256 with a salt per account and
  1000 rounds, which the ESP32 computes in well under a second.
  `crypto.rs` implements it without dependencies, and the tests compare it
  with python's hashlib
- signing up or logging in returns a session token, an hmac over the
  account id and password hash under a secret created on first boot. it
  needs no storage, survives reboots, and stops working when the password
  changes. the page keeps it in `localStorage` and sends it with requests
- at most 100 accounts are kept. the oldest one that is no admin makes
  room for a new one
- an account can be an admin, which the last field of its line in the
  `users` file marks. `is_admin` in `routes.rs` reads that bit off the
  session of a request, so there is no separate admin login. `POST
  /api/admin` lets an admin set or clear the bit of another account. the
  poll tells a client whether it is one, and the page then draws the pin
  and delete buttons and colors the account button

the network is open and unencrypted, so passwords and tokens can be read
by anyone in radio range. accounts keep honest people apart, no more.

## settings

`oasis.conf` is a small file of `key = value` lines and `[user]` and
`[thread]` sections, read by `seed.rs` without a dependency. it reaches a
device with the program: the firmware compiles it in (`include_str!`), the
raspberry pi image carries it in the initramfs, and the host binary reads
the file that `OASIS_CONFIG` names.

- `make` runs `oasis-host --check` on it before building for a device. a
  device that meets a file it cannot read logs that and starts with
  defaults, since refusing to start would leave nothing to connect to
- its accounts are applied at every start (`Users::build_in_all`): made if
  missing, and given the password and description of the file. the table
  is only written when that changed something. an account that the file
  makes an admin cannot lose the bit through `/api/admin`
- its threads are started once (`Portal::start_threads`). a `seeded` file
  in the data directory remembers that, so a thread that was deleted or
  evicted does not come back
- the network name is needed before the portal exists, so the platforms
  read it from the parsed file themselves

## mail

`mail.rs` keeps a mailbox per owner, a `Store` of two 4000 byte segments.
the owner is the account of a logged in client, or else the device id of
a guest. sending appends to the recipient's mailbox and a copy to the
sender's. the name of a mail record is the other party, and its reference
the direction: 0 for received, 1 for sent. mailboxes are opened per
request. only their ids and the unread counters, reported through the
poll, stay in RAM.

- mail to a guest is addressed to its generated name. the device keeps
  the 64 guests seen most recently in RAM to find the device behind a
  name. generated names can collide, in which case the guest seen last
  gets the mail. a guest's mailbox is only as private as its mac address
- at most 100 mailboxes are stored. the one unused for longest makes room
  for a new one, and a mailbox is deleted with its account

mail crosses the open network unencrypted, like everything else.

## onboarding

a full screen overlay in `index.html` has two pages. which one shows is
decided on the client:

- `welcome`, in a sign-in window: what the oasis offers and how to open it
  in a regular browser
- `join`, in a regular browser: the sign up form. it opens on the first
  visit of a device without an account and can be skipped. the account
  button in the top right opens the same form to change the profile

apple's sign-in sheet only offers its "done" once the OS connectivity check
succeeds, and closing it any other way drops the wifi. so in that sheet the
page calls `POST /api/release` on its first load and reloads, which prompts
the OS to check again. calling `window.close()` there is avoided: it
stopped iOS from offering "done".

android is not released automatically. a stock android sign-in window may
close by itself as soon as its check succeeds, before the page was read.
on GrapheneOS the window was observed to stay open even after its checks
got the success reply, so the page cannot make it close.

android's way out is `firmware/src/https.rs`: a TLS listener on port 443
with a self-signed certificate. the welcome page links to it. the sign-in
window reacts to the untrusted certificate by offering "continue anyway
via browser", which opens the link in the real browser. the browser warns
once more. whoever accepts is redirected to plain http. the listener
serves nothing else and handles one connection at a time, since a
handshake needs tens of KB of heap. since the dns answers every name with
our address, the port also gets the https traffic that phones mean for the
internet. `crates/portal/src/sni.rs` reads the server name of each client
hello, and the listener hangs up before the handshake unless the name is
ours or absent, as it is for `https://10.0.0.1/`. the certificate and key are created
per checkout by `make` (needs `openssl`) and are not in version control.
the page also tells android users how to do it by hand, and to pick "use
this network as is" if the portal does not load in their browser. from then on the
connectivity probes of that client get the success reply its OS expects
instead of a redirect, the OS marks the network as connected, and the
window can be closed. the client then believes it has internet. released
clients are kept in RAM, keyed by address and device.

## status and notifications

`GET /api/status` backs the status page. it lists the clients that have
the page open (the peers of the signaling table) and the devices that are
associated with the access point without it (`Config.stations`), reports
how full every store is against its limit, and passes on what the platform
says about its partitions (`Config.space`, in `firmware/src/space.rs`: the
firmware image, the data partition, the settings partition, and the heap).

notifications cost the device almost nothing. `events.rs` keeps the last
64 replies in RAM: topic, thread, reply id, who wrote it, and a one line
excerpt of 48 bytes. `mail.rs` keeps the same kind of notice for the last
64 unread mails of all mailboxes together, and sends a client the newest
8 of its own with the poll. the page draws one row per reply and per mail. a client
sends the id of the last one it saw with its poll and gets the newer ones.
which threads a visitor takes part in is only known to the page, which
remembers the threads it started or replied to in `localStorage` and turns
replies by others into notes. so notes belong to a browser, not to an
account, and replies made while nobody polled for 64 replies are missed.
unread mail is counted by the device and shown by the same button.

every name on the page is drawn by `nameNode` and opens the `user/<name>`
view, the profile, which has the button to send mail. guests have a
profile too, without a description.

## navigation

the part of the address after `#` names the place: `board`,
`board/<topic>`, `board/<topic>/<thread>`, `chat`, `mail`, `files`,
`account`, `status`, `notes`, or `user/<name>`. taps call `show(path)`, which pushes a history entry, and
`route()` draws whatever the address says. the browser's back button, the
arrow in the header, and typed or bookmarked addresses therefore all go
through the same code. the arrow uses `history.back()` when the current
place was reached from inside the page, and otherwise goes one level up, so
that it never leaves the oasis. sign up and the profile form are the
`account` view of the main page.

## writing

writing happens in a dock between the content and the tab bar, so it stays
at the bottom and above the keyboard. the forms for a new thread, a reply,
and a mail are closed by default: the dock then holds an action button at
its right. it opens the form that fits the current place as a panel with a
title line, a cross that closes it, and a send icon in the place of the
action button. a reply button on a mail or the "send mail" button of a
profile opens the mail panel addressed. sending closes the panel again.
chat keeps its panel open, since writing is all one does there.

what the device refuses, such as mail to an unknown name, is shown in a
small dialog with an okay button. while the device does not answer the
poll, the status button in the header is red, without a dialog or text.

## look of the page

all sizes and colors are custom properties and a handful of classes at the
top of the style block in `index.html` (R22). `--r` is the one corner
radius and `--accent` the one accent color. buttons come in three levels:
`.go` (filled accent, the main action), plain (an alternative), and
`.quiet` (outlined, a way to skip). the mark and the two header icons are
inline svg symbols, defined once and reused, so the page still loads in a
single request.

## layout of the page

the body is a flex column: header, scrolling main, nav at the bottom. its
height follows `visualViewport`, and the viewport meta asks browsers to
resize for the keyboard, so the nav and chat box stay above it.

flash layout (`firmware/partitions.csv`): 1.5MB app, no OTA slot (R2).

## time

the ESP32 has no clock. the first client that posts supplies its unix time
and the device counts from there. an admin's time overrides it. timestamps
are zero until then.

## peer to peer

`peers.rs` holds presence and a small signaling queue, both in RAM.

- a polling client registers with a secret `key` and receives a public id
- `POST /api/signal` queues an opaque message for another peer id, stamped
  with the sender's ip address
- signals are delivered once through the recipient's next poll

file sharing uses a WebRTC data channel. offers and answers are sent after
ICE gathering completes, so a transfer needs two signals. browsers hide
local addresses behind mDNS names in ICE candidates. the receiver replaces
those names with the address the device observed, so connections do not
depend on multicast working across the access point.

## raspberry pi

`crates/rpi` is one static binary (`aarch64-unknown-linux-musl`) that runs
as the init of a stock raspberry pi kernel. nothing else is on the card: a
shell, a service manager, and hostapd are all absent. linux provides what
ESP-IDF provides on the ESP32, `std::net` and `std::fs`, so the portal crate
is the same (R12).

`tools/rpi_image.py` builds the card image:

- partition 1, FAT: the pi's boot firmware, the kernel, `config.txt`,
  `cmdline.txt`, and the initramfs
- the initramfs: the binary as `/init`, the modules of the wifi driver
  (`brcmfmac` and what it depends on), the firmware of the wifi chip, and
  the certificate of the https listener
- partition 2, ext4, labeled `oasis`: the data. the init finds it by its
  label, since the kernel numbers disks in the order it finds them

kernel, modules, and chip firmware are downloaded by commit and checked
against `tools/rpi.lock`. the archive and both file systems are written
with fixed ids and timestamps, so the same inputs give the same image.

what the init does (`main.rs`), and with which part of the kernel:

- mounts `/dev`, `/proc`, `/sys` (`init.rs`)
- loads the wifi driver. `init.rs` reads `modules.dep` like modprobe does.
  the driver asks the kernel for a second, vendor specific module once it
  has identified the chip, and the kernel runs `/sbin/modprobe` for that,
  which is a link to the same binary
- turns the interface into an access point and starts an open network
  (`wifi.rs`, nl80211). the chip is "fullmac": its firmware beacons and
  associates clients, so three requests replace hostapd
- sets the address (`link.rs`, rtnetlink). `netlink.rs` is the socket
  protocol under both
- mounts the data partition
- starts `dhcp.rs`, `mdns.rs`, and `dns.rs` of the portal crate, then
  `https.rs` (rustls) and the portal

the kernel panics when init ends, so on an error the init logs it, waits,
and reboots.

`Config.mac_of` reads the kernel's arp table, which also knows clients that
configured their address by hand. `Config.stations` asks the wifi driver.

the limits follow the size of the data partition: half of it for the board,
a quarter for mail at 64KB per mailbox, and 10000 accounts. the rest covers
the block rounding of ext4, whose inodes are sized for one file per 4KB.

the settings file is `/oasis.conf` in the initramfs. two settings are also
words on the kernel command line (`cmdline.txt`), which can be edited on
the card without a rebuild: `oasis.ssid`, which goes before the file, and
`oasis.interface`. an interface that is not wifi, such as `eth0`, gets
the address and the services without an access point. the qemu test uses
`lo`, since qemu emulates neither the wifi chip nor the ethernet port.

## limits

the numbers of the ESP32. the raspberry pi has its own, see above.

| what                    | limit        |
|-------------------------|--------------|
| wifi clients            | 10           |
| http workers            | 4            |
| idle http connections   | 6            |
| chat history            | 50 messages  |
| chat message            | 280 bytes    |
| board post              | 2000 bytes   |
| accounts                | 100          |
| mailboxes               | 100          |
| mailbox                 | 8000 bytes   |
| mail message            | 1000 bytes   |
| peers                   | 16           |
| queued signals          | 12 x 4096 B  |
