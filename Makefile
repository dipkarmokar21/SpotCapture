.PHONY: build test run
build:
	./scripts/build.sh
test:
	./scripts/test.sh
run:
	./build/spotcapture-ui

