.PHONY: all check run build fmt clean

all: check build

check:
	cargo test --all-targets --all-features --locked
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings

run:
	cargo run

build:
	cargo build

fmt:
	cargo fmt --all

clean:
	cargo clean
