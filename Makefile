# every tool runs through mise so that versions come from mise.toml
MISE := mise exec --
PORT ?= /dev/ttyUSB0
BAUD ?= 921600
FLASH_BYTES := 0x400000
FIRMWARE := firmware/target/xtensa-esp32-espidf/release/oasis-firmware
ESP_ENV := .esp-env.sh
TLS_CERT := firmware/tls/cert.pem
TLS_DAYS := 7300
# the firmware directory selects the xtensa toolchain via rust-toolchain.toml
FIRMWARE_CARGO := . ./$(ESP_ENV) && cd firmware && $(MISE) cargo

.PHONY: build setup host firmware flash monitor backup run test-e2e test-browser precommit clean

build: host firmware

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

firmware: $(TLS_CERT)
	$(FIRMWARE_CARGO) build --release

flash: firmware
	$(MISE) espflash flash --port $(PORT) --baud $(BAUD) --partition-table firmware/partitions.csv $(FIRMWARE)

monitor:
	$(MISE) espflash monitor --port $(PORT)

# saves the full flash contents of the attached board before overwriting it
backup:
	$(MISE) espflash read-flash --port $(PORT) --baud $(BAUD) 0 $(FLASH_BYTES) backup-$(shell date +%Y%m%d-%H%M%S).bin

# serves the portal on http://127.0.0.1:8080/
run: host
	target/debug/oasis-host

test-e2e: host
	$(MISE) python -m unittest discover -s tests/e2e -v

# clicks through the page in a headless chrome, including a file transfer
# between two tabs. needs google-chrome and a network interface with a route
test-browser: host
	$(MISE) sh tests/browser/run.sh

precommit: $(TLS_CERT)
	$(MISE) cargo fmt --all --check
	$(MISE) cargo clippy --all-targets -- -D warnings
	cd firmware && $(MISE) cargo fmt --check
	$(FIRMWARE_CARGO) clippy --release -- -D warnings
	$(MISE) ruff check tests
	$(MISE) ruff format --check tests

clean:
	$(MISE) cargo clean
	cd firmware && $(MISE) cargo clean
