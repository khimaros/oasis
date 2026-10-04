# contributing

- product requirements live in `REQUIREMENTS.md`. do not regress on them
- architecture is described in `DESIGN.md`. update it with structural changes
- run every tool through mise (`mise exec -- ...` or the `make` targets) so
  that versions match `mise.toml`

## where code goes

anything that does not need hardware belongs in `crates/portal`, which must
stay std only with zero dependencies. `firmware` and `crates/rpi` only wire
up wifi and storage. this keeps features testable on the host.

## testing

features are tested end to end in `tests/e2e` (python, stdlib only) against
`oasis-host`. the host binary reads `OASIS_*` environment variables so tests
can shrink limits.

    make test-e2e
    make precommit

`make test-browser` clicks through the page in a headless chrome
(`tests/browser`): the board, sign up, chat, profiles, and a WebRTC file
transfer between two tabs. run it after every change to `index.html`. it
needs `google-chrome` and is not part of `make test-e2e`.

`make test-rpi` builds the raspberry pi image and boots it in qemu
(`tests/rpi`). qemu emulates the board and the sd card but no network
device, so the test follows the serial console up to the point where the
portal serves. run it after every change to `crates/rpi` or `tools`. it
needs `qemu-system-aarch64` and the tools of `make rpi-image`.

the wifi access point, captive portal behavior of real phones, and the
https listener are not covered by automated tests. check them on hardware with two clients after changing `index.html`,
`peers.rs`, `firmware`, or `crates/rpi`.

## ESP-IDF components

after changing `extra_components` in `firmware/Cargo.toml`, the bindings do
not rebuild on their own. run this in `firmware/` first:

    mise exec -- cargo clean --release -p esp-idf-sys

## memory

the device has roughly 150KB of free heap. every buffer, queue, and table
needs a fixed bound, declared as a constant at the top of its file.
