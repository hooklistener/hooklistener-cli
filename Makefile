.PHONY: all check check-cases run build fmt clean

all: check build

check:
	python3 scripts/release_workflow_contract_test.py
	python3 scripts/verify_cases_release_tests_test.py
	cargo test --all-targets --all-features --locked
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features --locked -- -D warnings

check-cases:
	python3 scripts/verify_cases_release_tests.py

run:
	cargo run

build:
	cargo build

fmt:
	cargo fmt --all

clean:
	cargo clean
