# export RETRO68=/path/to/Retro68-build/toolchain
RETRO68 ?= $(HOME)/hackplace/macfoo/Retro68-build/toolchain
CC=$(RETRO68)/bin/m68k-apple-macos-gcc
REZ=$(RETRO68)/bin/Rez
CFLAGS=-O2 -Wall -Wno-multichar -Wno-array-bounds
LDFLAGS=-lRetroConsole
RINCLUDES=$(RETRO68)/m68k-apple-macos/RIncludes
REZFLAGS=-I$(RINCLUDES)

all: ATalkTest.bin ATalkTest-800k.img

ATalkTest.bin: ATalkTest.code.bin
	$(REZ) $(REZFLAGS) \
		--copy "ATalkTest.code.bin" \
		"$(RINCLUDES)/Retro68APPL.r" \
		-t "APPL" -c "????" \
		-o ATalkTest.bin --cc "._ATalkTest.ad" --cc ATalkTest.dsk

# 800K HFS floppy image with the test program on it
ATalkTest-800k.img: ATalkTest.bin
	rm -f $@
	dd if=/dev/zero of=$@ bs=1024 count=800 status=none
	hformat -l ATalkTest $@ >/dev/null
	hcopy -m ATalkTest.bin :
	humount >/dev/null

ATalkTest.code.bin: atalktest.o atalk_listener.o
	$(RETRO68)/bin/m68k-apple-macos-g++ $^ -o $@ $(LDFLAGS)

atalktest.o: atalktest.c
	$(CC) $(CFLAGS) -c $< -o $@

atalk_listener.o: atalk_listener.s
	$(CC) -c $< -o $@

.PHONY: clean
clean:
	rm -f ATalkTest-800k.img ATalkTest.bin ATalkTest.code.bin \
		ATalkTest.code.bin.gdb *.o ATalkTest.ad ._ATalkTest.ad ATalkTest.dsk
