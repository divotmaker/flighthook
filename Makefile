headless ?= false

# Deploy target: `make deploy host=gamingpc dir=Code/flighthook`
# Falls back to env vars, then defaults.
host ?= $(or $(GOLF_SIMULATOR_REMOTE_HOST),localhost)
dir  ?= $(or $(GOLF_SIMULATOR_REMOTE_DIR),.)

# Build the UI WASM first, then the app (which embeds the WASM assets)
FEATURES := $(if $(filter true,$(headless)),--features headless)
WIN_TARGET := x86_64-pc-windows-gnu
DEPLOY_CARGO_FLAGS := $(if $(filter true,$(headless)),--no-default-features --features headless)

build: ui
	cd app && cargo build $(FEATURES)

release: ui
	cd app && cargo build --release $(FEATURES)

# Build the egui WASM dashboard
ui:
	cd ui && trunk build --release

test:
	cargo test --workspace

clippy:
	cargo clippy --workspace

build-windows: ui
	cd app && cargo build --release --target $(WIN_TARGET) $(DEPLOY_CARGO_FLAGS)

run: build
	cd app && cargo run $(FEATURES) -- $(if $(filter true,$(headless)),--headless) $(if $(config),--config $(config))

# Cross-compile for Windows, deploy to remote host.
# headless=false (default): builds with native GUI, deploys only.
# headless=true: builds without GUI, deploys and runs via SSH.
# Usage:
#   make deploy host=golfpc dir=Code/flighthook                # GUI binary, deploy only
#   make deploy host=golfpc dir=Code/flighthook headless=true  # headless, deploy + run over ssh in terminal
#   make deploy                                                # deploy to {GOLF_SIMULATOR_REMOTE_HOST}:{GOLF_SIMULATOR_REMOTE_DIR}
deploy: ui
	cd app && cargo build --release --target $(WIN_TARGET) $(DEPLOY_CARGO_FLAGS)
	@echo "==> deploying to $(host):$(dir)"
	ssh "$(host)" "mkdir $(dir) 2>nul & echo ok"
	scp target/$(WIN_TARGET)/release/flighthook.exe "$(host):$(dir)/"
	$(if $(filter true,$(headless)),@echo "==> starting flighthook on $(host)" && ssh -t "$(host)" "cd $(dir) && flighthook.exe --headless $(args)")

# Ensure Rust toolchain, wasm target, and trunk are installed
buildtools:
	@command -v cargo >/dev/null 2>&1 || { echo "Installing Rust via rustup..."; curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y; }
	@rustup target list --installed | grep -q wasm32-unknown-unknown || { echo "Adding wasm32 target..."; rustup target add wasm32-unknown-unknown; }
	@command -v trunk >/dev/null 2>&1 || { echo "Installing trunk..."; cargo install trunk; }
	@command -v cargo-clippy >/dev/null 2>&1 || { echo "Installing clippy..."; rustup component add clippy; }
	@rustup target list --installed | grep -q x86_64-pc-windows-gnu || { echo "Adding Windows cross-compile target..."; rustup target add x86_64-pc-windows-gnu; }
	@command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 || { echo "ERROR: mingw-w64 not installed. Run: sudo apt install mingw-w64"; exit 1; }

stop:
	@pkill -x flighthook 2>/dev/null && echo "stopped" || echo "nothing running"

# Publish schemas + lib to crates.io. Dry-runs both first to catch errors
# before committing any real publish.
publish:
	cargo publish -p flighthook --dry-run
	cargo publish -p flighthook

clean:
	cd app && cargo clean
	rm -rf ui/dist

.PHONY: build build-windows release ui test clippy run deploy stop clean buildtools publish
