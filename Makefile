.PHONY: build-debug build-release install

build-debug: target/debug/sway-scratchpads

target/debug/sway-scratchpads:
	cargo build

build-release: target/release/sway-scratchpads

target/release/sway-scratchpads:
	cargo build --release

install: build-release
	cargo install --path=. --target-dir=target
