# Rules shared by the testers, to build one Retro68 (m68k) application and an HFS floppy image
# carrying it. A tester's Makefile sets these, then includes this file:
#
#   APP        the program's name: builds $(APP).bin and $(APP)-$(FLOPPY_KB)k.img
#   OBJS       its object files, from .c or .s files of the same name
#   LDFLAGS    libraries (optional), e.g. -lRetroConsole
#   REZ_SRC    its resource file (optional), by default Retro68's standard APPL resources
#   FLOPPY_KB  the size of the floppy image in KB (optional, 800 by default)
#
# Set RETRO68 to the Retro68 toolchain: export RETRO68=/path/to/Retro68-build/toolchain

ifneq ($(filter-out clean,$(or $(MAKECMDGOALS),all)),)
ifndef RETRO68
$(error Set RETRO68 to the Retro68 toolchain root)
endif
endif

CC = $(RETRO68)/bin/m68k-apple-macos-gcc
CXX = $(RETRO68)/bin/m68k-apple-macos-g++
REZ = $(RETRO68)/bin/Rez
CFLAGS = -O2 -Wall -Wno-multichar -Wno-array-bounds
RINCLUDES = $(RETRO68)/m68k-apple-macos/RIncludes
REZFLAGS = -I$(RINCLUDES)
REZ_SRC ?= $(RINCLUDES)/Retro68APPL.r
FLOPPY_KB ?= 800
FLOPPY = $(APP)-$(FLOPPY_KB)k.img

all: $(APP).bin $(FLOPPY)

$(APP).bin: $(APP).code.bin $(filter-out $(RINCLUDES)/%,$(REZ_SRC))
	$(REZ) $(REZFLAGS) \
		--copy "$(APP).code.bin" \
		"$(REZ_SRC)" \
		-t "APPL" -c "????" \
		-o $@ --cc "._$(APP).ad" --cc $(APP).dsk

$(APP).code.bin: $(OBJS)
	$(CXX) $^ -o $@ $(LDFLAGS)

%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

%.o: %.s
	$(CC) -c $< -o $@

# HFS floppy image with the program on it
$(FLOPPY): $(APP).bin
	rm -f $@
	dd if=/dev/zero of=$@ bs=1024 count=$(FLOPPY_KB) status=none
	hformat -l $(APP) $@ >/dev/null
	hcopy -m $(APP).bin :
	humount >/dev/null

.PHONY: all clean
clean:
	rm -f $(FLOPPY) $(APP).bin $(APP).code.bin $(APP).code.bin.gdb $(OBJS) \
		$(APP).ad ._$(APP).ad $(APP).dsk
