# every tool runs through mise so that versions come from mise.toml
MISE := mise exec --
PORT ?= /dev/ttyUSB0
BAUD ?= 921600
FLASH_BYTES := 0x400000
# not a pinned tool: only `make phone-wifi` uses it, on whatever phone is at hand
ADB ?= adb
# label of the user data in firmware/partitions.csv
STORAGE_PARTITION := storage
FIRMWARE := firmware/target/xtensa-esp32-espidf/release/oasis-firmware
ESP_ENV := .esp-env.sh
TLS_CERT := firmware/tls/cert.pem
TLS_DAYS := 7300
# the firmware directory selects the xtensa toolchain via rust-toolchain.toml
FIRMWARE_CARGO := . ./$(ESP_ENV) && cd firmware && $(MISE) cargo
RPI_TARGET := aarch64-unknown-linux-musl
RPI_BINARY := target/$(RPI_TARGET)/release/oasis-rpi
RPI_OUT := target/rpi
RPI_CACHE := .cache/rpi
# megabytes of the data partition. the limits of the portal grow with it
RPI_DATA_MB ?= 2048
# the settings that are flashed with the program: the network name, the
# accounts, and the first threads. see oasis.conf.example
CONFIG := oasis.conf
HOST := target/debug/oasis-host

.PHONY: build setup host check-config firmware rpi rpi-image flash wipe flash-wipe phone-wifi monitor backup run test-e2e test-browser test-rpi precommit clean

build: host firmware rpi

# one time install of the pinned toolchain
setup:
	mise trust
	mise install
	$(MISE) sh -c 'espup install --std --targets esp32 --toolchain-version $$ESP_RUST_VERSION --export-file $(ESP_ENV)'

host:
	$(MISE) cargo build

# self-signed certificate for the https listener. it is meant to be
# untrusted, see firmware/src/https.rs. keep it ECDSA: with an RSA one,
# android's sign-in window no longer offered "continue anyway via browser". created once per checkout so that
# the private key never enters version control.
$(TLS_CERT):
	mkdir -p $(dir $(TLS_CERT))
	openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days $(TLS_DAYS) \
		-subj "/CN=oasis" -keyout $(dir $(TLS_CERT))key.pem -out $(TLS_CERT)

# created once per checkout, then yours to edit. it holds passwords, so it
# stays out of version control
$(CONFIG):
	cp $(CONFIG).example $(CONFIG)

# a device ignores a settings file that it cannot read, so refuse to build one in
check-config: host $(CONFIG)
	$(HOST) --check $(CONFIG)

firmware: $(TLS_CERT) check-config
	$(FIRMWARE_CARGO) build --release

# static binary for the raspberry pi 4. the crypto library of its https
# listener has parts in C, which the host's clang compiles for arm
rpi:
	CC_aarch64_unknown_linux_musl=clang AR_aarch64_unknown_linux_musl=llvm-ar \
		$(MISE) cargo build --release -p oasis-rpi --target $(RPI_TARGET)

# sd card image, written to $(RPI_OUT)/oasis-rpi4.img. needs mkfs.vfat,
# mcopy, and mke2fs. e.g. `RPI_DATA_MB=16000 make rpi-image`
rpi-image: rpi $(TLS_CERT) check-config
	$(MISE) python tools/rpi_image.py --binary $(RPI_BINARY) --config $(CONFIG) --tls $(dir $(TLS_CERT)) \
		--cache $(RPI_CACHE) --out $(RPI_OUT) --data-mb $(RPI_DATA_MB)

flash: firmware
	$(MISE) espflash flash --port $(PORT) --baud $(BAUD) --partition-table firmware/partitions.csv $(FIRMWARE)

# erases the user data of the attached board: accounts, board, and mail. the
# board restarts, formats the partition, and applies the settings again
wipe:
	$(MISE) espflash erase-parts --port $(PORT) --non-interactive \
		--partition-table firmware/partitions.csv $(STORAGE_PARTITION)

# a board as new: the firmware first, so that it is the one to start on the
# empty partition
flash-wipe: flash wipe

# brings the wifi of an android phone on usb back after its wifi stack shut
# itself down, see docs/grapheneos-wifi.md. adb takes the phone and the
# server from ANDROID_SERIAL and ANDROID_ADB_SERVER_PORT
phone-wifi:
	$(ADB) shell cmd wifi set-wifi-enabled enabled

monitor:
	$(MISE) espflash monitor --port $(PORT)

# saves the full flash contents of the attached board before overwriting it
backup:
	$(MISE) espflash read-flash --port $(PORT) --baud $(BAUD) 0 $(FLASH_BYTES) backup-$(shell date +%Y%m%d-%H%M%S).bin

# serves the portal on http://127.0.0.1:8080/
run: host $(CONFIG)
	OASIS_CONFIG=$(CONFIG) $(HOST)

test-e2e: host
	$(MISE) python -m unittest discover -s tests/e2e -v

# clicks through the page in a headless chrome, including a file transfer
# between two tabs. needs google-chrome and a network interface with a route
test-browser: host
	$(MISE) sh tests/browser/run.sh

# boots the sd card image in qemu, which emulates the pi but not its wifi.
# needs qemu-system-aarch64
test-rpi: rpi-image
	$(MISE) python -m unittest discover -s tests/rpi -v

precommit: $(TLS_CERT) $(CONFIG)
	$(MISE) cargo fmt --all --check
	$(MISE) cargo clippy --all-targets -- -D warnings
	cd firmware && $(MISE) cargo fmt --check
	$(FIRMWARE_CARGO) clippy --release -- -D warnings
	$(MISE) ruff check tests tools
	$(MISE) ruff format --check tests tools

clean:
	$(MISE) cargo clean
	cd firmware && $(MISE) cargo clean
