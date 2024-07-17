.PHONY: aeronmd
aeronmd: # run the aeron media driver used for integration tests
	sudo rm -rf /dev/shm/aeron-default/
	docker run --rm --ipc=host  -u $(shell id -u ${USER}):$(shell id -g ${USER}) neomantra/aeron-cpp-debian:latest

.PHONY: build
build: # build with all features enabled
	cargo build --all-features

.PHONY: help
help: # Show the generated help message
	@grep -E '^[a-zA-Z0-9 -\.]+:.*#'  Makefile | while read -r l; do printf "\033[1;32m$$(echo $$l | cut -f 1 -d':')\033[00m:$$(echo $$l | cut -f 2- -d'#')\n"; done | sort

.PHONY: test
test: # run all tests with all features enabled
	cargo test --all-features
