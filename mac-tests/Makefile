# mac-tests: the guest testers (afp/, scsi/) and the headless runner that boots them (runner/).
# The testers need RETRO68 set to the Retro68 toolchain; the runner doesn't.

TESTERS = afp scsi

all: $(TESTERS)

$(TESTERS):
	$(MAKE) -C $@

# The runner, plain and with the test features of the built-in AppleTalk node. Each has a target
# directory of its own, so that building one doesn't rebuild snow_core for the other. Cargo
# rebuilds them when snow_core changes.
runner:
	cargo build --release --manifest-path runner/Cargo.toml --target-dir runner/target/plain

runner-afp:
	cargo build --release --manifest-path runner/Cargo.toml --target-dir runner/target/afp \
		--features afp

.PHONY: all clean runner runner-afp $(TESTERS)
clean:
	for t in $(TESTERS); do $(MAKE) -C $$t clean; done
