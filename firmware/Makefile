# OpenBoot build entry point: one Makefile, an eight-image matrix.
#
# OpenBoot is an 8 KiB IAP bootloader living in flash [0x0000, 0x2000) on WCH
# CH570/CH572/CH591/CH592 parts; the application starts at 0x2000. One image
# is built per CHIP x TRANSPORT combination:
#
#   CHIP      = ch570 | ch572 | ch591 | ch592
#   TRANSPORT = usb | uart        (exactly one transport is linked per image)
#
#   make                            image for the default CHIP/TRANSPORT
#   make CHIP=ch570 TRANSPORT=usb   one specific image
#   make ch570-usb                  the same image, as a convenience target
#   make all                        the full 8-image matrix
#   make matrix-report              the matrix plus a size table
#   make flash                      write the current image via minichlink (SWD)
#   make flash-isp                  write the current image via wchisp (ROM ISP)
#   make test                       host-native core tests, no cross toolchain
#   make check-deps                 verify pinned SDKs + toolchain
#   make print-image-path           absolute path of the current .bin, nothing else
#
# Every image must fit the 8 KiB bootloader region. That is enforced three
# ways: the linker script's FLASH MEMORY length, the post-objcopy byte gate in
# the .bin rule below, and the matrix-report table.

# Grouped targets and other modern behaviors need GNU Make 4.3. An older Make
# misparses the syntax and does something silently wrong instead of refusing,
# so refuse up front rather than produce artifacts nobody can explain.
ifneq ($(shell printf '4.3\n%s\n' '$(MAKE_VERSION)' | sort -V -C && echo ok),ok)
  $(error GNU Make 4.3 or newer is required; this is $(MAKE_VERSION))
endif

SHELL := /bin/sh
.SHELLFLAGS := -eu -c
# Delete a target whose recipe fails. Load-bearing for the size gate below: an
# oversize .bin must not survive on disk where a later `make flash` would
# happily program it over the bootloader region.
.DELETE_ON_ERROR:

# ---------------------------------------------------------------------------
# The build matrix.
#
# CHIP selects the flash geometry (app end address) and the HELLO family id.
# TRANSPORT selects the one transport .c linked into the image and the CPU
# clock: UART images run entirely at the 6.4 MHz boot clock (115200 baud fits
# with DL=7, -0.79% error, and libISP flash ops are proven to work there), so
# they need no clock init at all. USB images use the PLL: 100 MHz on ch57x
# and 60 MHz on ch59x.
#
# The default is CH592 UART, which leaves the debugger enabled.
# ---------------------------------------------------------------------------
CHIPS := ch570 ch572 ch591 ch592
TRANSPORTS := usb uart

CHIP ?= ch592
TRANSPORT ?= uart

ifeq ($(filter $(CHIP),$(CHIPS)),)
  $(error CHIP must be one of {$(CHIPS)}, got "$(CHIP)")
endif
ifeq ($(filter $(TRANSPORT),$(TRANSPORTS)),)
  $(error TRANSPORT must be one of {$(TRANSPORTS)}, got "$(TRANSPORT)")
endif

# Chip -> port family. CH570/CH572 share the ch57x port, CH591/CH592 the
# ch59x port; everything variant-specific below the family level is injected
# as a -D (see DEFINES) so the port sources stay variant-agnostic.
PORT_ch570 := ch57x
PORT_ch572 := ch57x
PORT_ch591 := ch59x
PORT_ch592 := ch59x
PORT := $(PORT_$(CHIP))

# Exclusive end of the application flash region, injected per VARIANT as
# -DOB_FLASH_APP_END (openboot_port.h #errors if it is missing):
#   ch570/ch572: 240 K part, app [0x2000, 0x3B000) (top 4 K block at 0x3B000
#                holds the boot record -- these parts have no DataFlash)
#   ch591:       192 K part, app [0x2000, 0x30000)
#   ch592:       448 K part, app [0x2000, 0x70000)
APP_END_ch570 := 0x0003B000
APP_END_ch572 := 0x0003B000
APP_END_ch591 := 0x00030000
APP_END_ch592 := 0x00070000
APP_END := $(APP_END_$(CHIP))

# HELLO chip-family id (names from protocol/openboot_protocol.h).
FAMILY_ch570 := OB_FAMILY_CH570
FAMILY_ch572 := OB_FAMILY_CH572
FAMILY_ch591 := OB_FAMILY_CH591
FAMILY_ch592 := OB_FAMILY_CH592
CHIP_FAMILY := $(FAMILY_$(CHIP))

# HELLO transport id and the CPU clock the image runs at (rationale above).
TRANSPORT_ID_usb := OB_TRANSPORT_ID_USB
TRANSPORT_ID_uart := OB_TRANSPORT_ID_UART
TRANSPORT_ID := $(TRANSPORT_ID_$(TRANSPORT))
# USB clock is per FAMILY, not shared: every WCH EVT USB example for ch57x
# runs CLK_SOURCE_HSE_PLL_100MHz (0x46), and at 60 MHz the ch570 PHY
# enumerates but is electrically unstable (hub reports "disabled by hub
# (EMI?)" and redetects it as low-speed). ch59x USB remains at 60 MHz.
CPU_HZ_usb_ch57x := 100000000
CPU_HZ_usb_ch59x := 60000000
CPU_HZ_uart_ch57x := 6400000
CPU_HZ_uart_ch59x := 6400000
CPU_HZ := $(CPU_HZ_$(TRANSPORT)_$(PORT))

# RAM geometry for the linker. The Makefile owns the per-family RAM size
# (it already owns the per-chip flash geometry above); the top-of-RAM
# bootreq address is NOT duplicated here — it is extracted from the
# protocol header when memory.ld is generated, so the two can never drift
# apart silently (the linker ASSERT compares them).
RAM_SIZE_ch57x := 12K
RAM_SIZE_ch59x := 26K
RAM_SIZE := $(RAM_SIZE_$(PORT))
BOOTREQ_NAME_ch57x := OB_BOOTREQ_ADDR_CH57X
BOOTREQ_NAME_ch59x := OB_BOOTREQ_ADDR_CH59X
BOOTREQ_NAME := $(BOOTREQ_NAME_$(PORT))
PROTOCOL_H := $(abspath ../protocol/openboot_protocol.h)
REPO_ROOT := $(abspath ..)

# Pinned openwch EVT trees (git submodules; check-deps verifies the pins).
# Only headers and libISP come from here -- no SDK driver .c is compiled.
SDK_ch57x := $(abspath ../third_party/openwch/ch570/EVT/EXAM)
SDK_ch59x := $(abspath ../third_party/openwch/ch592/EVT/EXAM)
SDK ?= $(SDK_$(PORT))

# The ROM flash API archive. This is the ONLY code linked from the SDK.
ISPLIB_ch57x := ISP572
ISPLIB_ch59x := ISP592
ISPLIB := $(ISPLIB_$(PORT))
ISPLIB_A := $(SDK)/SRC/StdPeriphDriver/lib$(ISPLIB).a

# ---------------------------------------------------------------------------
# Board configuration. A board .mk provides the boot-strap pin and the idle
# auto-boot timeout; BOARD is overridable so real hardware can ship its own
# file without touching this Makefile:  make BOARD=myboard CHIP=ch592 ...
# ---------------------------------------------------------------------------
BOARD ?= generic-$(PORT)
include boards/$(BOARD).mk
# Fallback if a board file omits the timeout; 0 disables idle auto-boot.
OB_IDLE_TIMEOUT_MS ?= 10000
# A board that defines only a strap mask selects port A by default.
OB_BOOT_PIN_PORT_B ?= 0

# ---------------------------------------------------------------------------
# Toolchain: a MounRiver "RISC-V Embedded GCC" install. The caller must point
# MRS_TOOLCHAIN at its bin directory. Every recipe use is quoted so paths may
# contain spaces (the stock install directories do):
#   make MRS_TOOLCHAIN=/path/to/bin ...
#
# GCC12 and GCC15 both build the matrix; check-deps warns rather than refuses
# on anything else. Note GCC15 renamed the tool prefix and emits slightly
# different code — see the TOOL_PREFIXES probe below and firmware/README.md.
# ---------------------------------------------------------------------------
MRS_TOOLCHAIN ?=

# Host-native tests and clean do not need the cross-toolchain. All public
# firmware targets do, including the default goal and matrix convenience
# targets, so fail at parse time with one actionable message.
TOOLCHAIN_GOALS := image all matrix-report board-policy check-deps flash flash-isp \
                   $(foreach c,$(CHIPS),$(foreach t,$(TRANSPORTS),$(c)-$(t)))
ifeq ($(strip $(MAKECMDGOALS)),)
  NEEDS_TOOLCHAIN := 1
else ifneq ($(strip $(filter $(TOOLCHAIN_GOALS),$(MAKECMDGOALS))),)
  NEEDS_TOOLCHAIN := 1
endif
ifeq ($(NEEDS_TOOLCHAIN),1)
  ifeq ($(strip $(MRS_TOOLCHAIN)),)
    $(error MRS_TOOLCHAIN is required; set it to a MounRiver RISC-V Embedded GCC bin directory)
  endif
endif

# Preserve the caller's host-compiler choice before CC becomes the
# cross-compiler. HOST_CC wins; an incoming CC is the compatibility fallback.
ifeq ($(strip $(HOST_CC)),)
  ifneq ($(filter default undefined,$(origin CC)),)
    HOST_CC := cc
  else
    HOST_CC := $(CC)
  endif
endif
export HOST_CC

# MounRiver renamed the tool prefix at GCC15: riscv-wch-elf-* became
# riscv32-wch-elf-*. Detect it instead of making every contributor bridge the
# rename with a symlink farm. Probed with $(shell) rather than $(wildcard)
# because MRS_TOOLCHAIN routinely contains a space ("RISC-V Embedded GCC12")
# and $(wildcard) splits on whitespace. Kept in sync with detect_tool_prefix()
# in check_dependencies.py, which enforces the same list.
TOOL_PREFIXES := riscv-wch-elf- riscv32-wch-elf-
ifneq ($(strip $(MRS_TOOLCHAIN)),)
  TOOL_PREFIX := $(shell for p in $(TOOL_PREFIXES); do \
                     if [ -x "$(MRS_TOOLCHAIN)/$${p}gcc" ]; then \
                       printf '%s' "$$p"; break; \
                     fi; \
                   done)
  ifeq ($(NEEDS_TOOLCHAIN)$(strip $(TOOL_PREFIX)),1)
    $(error no MounRiver compiler in "$(MRS_TOOLCHAIN)" (expected one of \
            $(addsuffix gcc,$(TOOL_PREFIXES))))
  endif
endif

CC := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)gcc
AS := $(CC)
LD := $(CC)
OBJCOPY := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)objcopy
SIZE := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)size
NM := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)nm

MINICHLINK ?= minichlink
WCHLINK_SERIAL ?=
WCHISP ?= wchisp
# Hermetic interpreter for the dependency gate; plain python3 for the host
# test harness, which needs the user's pytest.
PYTHON ?= /usr/bin/python3 -I
HOST_PYTHON ?= python3

# ---------------------------------------------------------------------------
# Artifact names: build/<chip>-<transport>/openboot-<chip>-<transport>.{elf,
# bin,hex,map}. Each matrix cell gets its own directory, so the whole matrix
# can coexist (and `make -j all` can build it in parallel).
# ---------------------------------------------------------------------------
# $(call build_dir,<chip>,<transport>,<board>) — the ONE place that knows the
# artifact layout. A non-default board gets its own directory so it can never
# reuse the default board's objects; the default keeps the documented
# build/<chip>-<transport> path unchanged.
build_dir = build/$(1)-$(2)$(if $(filter-out generic-$(PORT_$(1)),$(3)),+$(3))

TARGET := openboot-$(CHIP)-$(TRANSPORT)
BUILD := $(call build_dir,$(CHIP),$(TRANSPORT),$(BOARD))
MAX_IMAGE_BYTES := 8192

# ---------------------------------------------------------------------------
# Sources. The core is chip-agnostic (no SDK headers); one port and exactly
# one transport are compiled per image.
# ---------------------------------------------------------------------------
CORE_SRC := core/main.c core/crc32.c core/boot_core.c core/boot_decision.c
PORT_SRC := ports/$(PORT)/port_$(PORT).c
TRANSPORT_SRC := transports/$(TRANSPORT)_transport.c
C_SRCS := $(CORE_SRC) $(PORT_SRC) $(TRANSPORT_SRC)
S_SRCS := ports/$(PORT)/startup_openboot.S

# Objects are flattened into $(BUILD) by basename and found via VPATH; the
# guard keeps a future duplicate basename from silently colliding there.
C_OBJS := $(patsubst %.c,$(BUILD)/%.o,$(notdir $(C_SRCS)))
S_OBJS := $(patsubst %.S,$(BUILD)/%.o,$(notdir $(S_SRCS)))
OBJS := $(C_OBJS) $(S_OBJS)
DEPS := $(OBJS:.o=.d)

SOURCE_BASENAMES := $(notdir $(C_SRCS) $(S_SRCS))
ifneq ($(words $(SOURCE_BASENAMES)),$(words $(sort $(SOURCE_BASENAMES))))
  $(error duplicate source basename in the flattened object directory)
endif

VPATH := core:ports/$(PORT):transports

# ---------------------------------------------------------------------------
# Flags.
# ---------------------------------------------------------------------------
# ch57x is a QingKe V3C-class core with the WCH extension set; ch59x is a
# plain RV32IMAC QingKe V4C. -msave-restore shares prologue/epilogue
# millicode from libgcc, reducing the image by more than 100 bytes.
ARCH_ch57x := -march=rv32imc_zba_zbb_zbc_zbs_xw -mabi=ilp32 -mcmodel=medany \
              -msmall-data-limit=8 -msave-restore
ARCH_ch59x := -march=rv32imac_zicsr_zifencei -mabi=ilp32 \
              -msmall-data-limit=8 -msave-restore
ARCH := $(ARCH_$(PORT))

# NO -flto, deliberately: the image links against the binary-only libISP
# archives from the EVT tree, which were not built for LTO. Mixing LTO
# objects with opaque archives risks symbol and section surprises in the
# .highcode flash primitives that must run from RAM.
CFLAGS := $(ARCH) -Os -g -std=gnu99
CFLAGS += -Wall -Wextra -Werror
CFLAGS += -ffunction-sections -fdata-sections -fno-common

INCLUDES := -I core -I ports/$(PORT)
# SFR + ISP headers only; no SDK driver sources are ever compiled.
INCLUDES += -I $(SDK)/SRC/StdPeriphDriver/inc

# The configuration contract (see core/openboot_port.h, which #errors when
# any of these is missing). Every value is emitted into a generated header
# that is force-included ahead of every translation unit, so no source file
# needs to include it and the host test harness — which supplies its own
# flags — is unaffected:
#   OB_PORT_HEADER   which port header the core includes.
#   OB_FLASH_APP_END per chip VARIANT (the port covers a family).
#   OB_CPU_HZ        per transport (boot clock vs PLL).
#   OB_CHIP_FAMILY   HELLO family id for this variant.
#   OB_TRANSPORT_ID  HELLO transport id for this image.
# The Boolean board knobs further below are #ifdef-tested, so a knob left
# unset (or set to a literal 0) must emit NO line at all.
CONFIG_H := $(BUILD)/openboot_config.h

CONFIG_LINES := \
  '/* Generated by firmware/Makefile for $(CHIP)-$(TRANSPORT), board $(BOARD). DO NOT EDIT. */' \
  '\#define OB_PORT_HEADER "port_$(PORT).h"' \
  '\#define OB_FLASH_APP_END $(APP_END)' \
  '\#define OB_CPU_HZ $(CPU_HZ)' \
  '\#define OB_CHIP_FAMILY $(CHIP_FAMILY)' \
  '\#define OB_TRANSPORT_ID $(TRANSPORT_ID)' \
  '\#define OB_IDLE_TIMEOUT_MS $(OB_IDLE_TIMEOUT_MS)'
# CH57x crystal load capacitance. Leave this absent to use the port's
# vendor-reference default; emit an explicit board override when supplied.
ifneq ($(strip $(OB_HSE_CAP_LOAD)),)
ifneq ($(PORT),ch57x)
$(error OB_HSE_CAP_LOAD is only valid for CH57x)
endif
ifneq ($(words $(strip $(OB_HSE_CAP_LOAD))),1)
$(error OB_HSE_CAP_LOAD must be one value from 0 to 7)
endif
ifeq ($(filter 0 1 2 3 4 5 6 7,$(strip $(OB_HSE_CAP_LOAD))),)
$(error OB_HSE_CAP_LOAD must be one value from 0 to 7)
endif
CONFIG_LINES += '\#define OB_HSE_CAP_LOAD $(OB_HSE_CAP_LOAD)'
endif
# A board that wants no strap leaves OB_BOOT_PIN_MASK unset and the port
# compiles ob_bootpin_asserted() to `return 0`.
ifneq ($(strip $(OB_BOOT_PIN_MASK)),)
CONFIG_LINES += '\#define OB_BOOT_PIN_MASK $(OB_BOOT_PIN_MASK)' \
                '\#define OB_BOOT_PIN_PORT_B $(OB_BOOT_PIN_PORT_B)'
endif
# UART1 pin remap (ch59x only): RXD1_/TXD1_ on PB12/PB13 instead of PA8/PA9.
ifneq ($(filter-out 0,$(strip $(OB_UART1_REMAP))),)
CONFIG_LINES += '\#define OB_UART1_REMAP 1'
endif
# Optional full image-CRC check at every boot (boot_decision.c #ifdef;
# ~0.5 s at the 6.4 MHz boot clock, see README knob table).
ifneq ($(filter-out 0,$(strip $(OB_BOOT_IMAGE_CRC))),)
CONFIG_LINES += '\#define OB_BOOT_IMAGE_CRC 1'
endif

# Force-include the generated configuration ahead of every TU. Equivalent to
# the -D forest it replaces: both establish the macros before line 1 of the
# source, so the object code is unchanged (the byte-identity gate in the
# commit message proves it). CFLAGS is simply-expanded, so this must come
# after CONFIG_H is defined.
CFLAGS += -include $(CONFIG_H)

LDFLAGS := $(ARCH) -T ports/openboot.ld -nostartfiles
LDFLAGS += --specs=nano.specs --specs=nosys.specs
LDFLAGS += -Wl,--gc-sections -Wl,--print-memory-usage
LDFLAGS += -Wl,-Map=$(BUILD)/$(TARGET).map
LDFLAGS += -L $(BUILD)          # INCLUDE memory.ld
LDFLAGS += -L $(SDK)/SRC/StdPeriphDriver
LDLIBS := -l$(ISPLIB)

# ---------------------------------------------------------------------------
# Targets.
# ---------------------------------------------------------------------------
.DEFAULT_GOAL := image
MATRIX := $(foreach c,$(CHIPS),$(foreach t,$(TRANSPORTS),$(c)-$(t)))
.PHONY: board-policy image all matrix-report clean check-deps test flash flash-isp \
        print-image-path $(MATRIX)

image: check-deps $(BUILD)/$(TARGET).bin $(BUILD)/$(TARGET).hex \
       $(BUILD)/$(TARGET).manifest

# Where the current CHIP/TRANSPORT/BOARD image lands, for consumers that
# package the bootloader (a superproject composing a factory image, say).
# The build_dir template above is the only definition of that layout, and a
# consumer hardcoding it breaks silently the day the template changes.
#
# Contract: prints one absolute path and NOTHING else, builds nothing, and is
# deliberately absent from TOOLCHAIN_GOALS so it answers without
# MRS_TOOLCHAIN set. The .elf/.hex/.map sit beside it under the same stem.
# Callers must pass --no-print-directory, because make's own "Entering
# directory" lines go to stdout and would otherwise land in $(shell ...).
print-image-path:
	@echo "$(abspath $(BUILD)/$(TARGET).bin)"

# The full matrix, one recursive make per cell so each gets its own CHIP/
# TRANSPORT (and thus its own flag set and build directory). The `+` marks
# the recursion for the jobserver, so `make -j8 all` parallelizes across
# cells and `make -n all` dry-runs into them.
all: $(MATRIX)

$(MATRIX):
	+$(MAKE) --no-print-directory \
	    CHIP=$(firstword $(subst -, ,$@)) \
	    TRANSPORT=$(lastword $(subst -, ,$@)) \
	    MRS_TOOLCHAIN="$(MRS_TOOLCHAIN)" image

# Size table over the whole matrix -- the third leg of the size gate, and
# the number to watch while the USB transport grows. Paths come from the
# same build_dir template the per-cell builds use, so the layout is defined
# in exactly one place.
# A BOARD= given on the command line reaches the recursive per-cell makes
# through MAKEFLAGS, so those cells build into build/<chip>-<transport>+<board>.
# The report has to look where the build actually went, or it reads whatever
# stale default-board binaries happen to be lying around and reports sizes for
# images it did not just build.
matrix_board = $(if $(filter command line,$(origin BOARD)),$(BOARD),generic-$(PORT_$(1)))
MATRIX_BINS := $(foreach c,$(CHIPS),$(foreach t,$(TRANSPORTS),\
  $(call build_dir,$(c),$(t),$(call matrix_board,$(c)))/openboot-$(c)-$(t).bin))

# No family ships a stay-in-bootloader strap — the mask-ROM ISP entry pin is
# the recovery mechanism (PA1, also USB D+, on ch57x; PB22 on ch59x). Getting
# that wrong is silent on the bench: a strap on a pin the package does not
# bond just reads its own pull-up and never asserts. So it is checked in the
# built artifact rather than the board file, right where the whole matrix is
# already in hand.
board-policy: all
	@$(HOST_PYTHON) check_board_policy.py --nm "$(NM)" --build build --board "$(BOARD)"

matrix-report: all board-policy
	@echo; \
	 printf '%-22s %s\n' "image" "bytes / limit"; \
	 printf '%s\n' "-------------------------------------"; \
	 for b in $(MATRIX_BINS); do \
	   n="$$(basename "$$b" .bin)"; n="$${n#openboot-}"; \
	   if [ -f "$$b" ]; then \
	     printf '%-22s %5s / $(MAX_IMAGE_BYTES)\n' "$$n" "$$(wc -c < "$$b")"; \
	   else \
	     printf '%-22s %s\n' "$$n" "missing"; \
	   fi; \
	 done

check-deps:
	$(PYTHON) check_dependencies.py --toolchain "$(MRS_TOOLCHAIN)"

# Host-native tests: compile the real core/*.c against a simulated flash and
# drive it from pytest. Needs a host C compiler and pytest, but no cross
# toolchain and no hardware.
test:
	$(HOST_PYTHON) ../protocol/gen_protocol.py --check
	$(HOST_PYTHON) tests/core_host/build.py
	$(HOST_PYTHON) -m pytest tests -q

$(BUILD):
	mkdir -p "$@"

# The generated config header IS the configuration, so every object depends
# on it and any knob change rebuilds everything. The recipe runs on every
# invocation but rewrites the file only when its contents actually change,
# so the mtime — and therefore the rebuild — is content-driven rather than
# timestamp-driven. That replaces the old parse-time content stamp: no
# $(file)/$(shell) side effects during Makefile evaluation.
.PHONY: FORCE
FORCE:

$(CONFIG_H): FORCE | $(BUILD)
	@printf '%s\n' $(CONFIG_LINES) > "$@.tmp"
	@if cmp -s "$@.tmp" "$@"; then rm -f "$@.tmp"; else mv -f "$@.tmp" "$@"; fi

# The linker's half of the generated configuration: the MEMORY block plus
# OB_BOOTREQ_ADDR, extracted from the protocol header (anchored sed, hard
# failure if the define ever moves or is renamed). FORCE + cmp for the same
# reason as the config header: the content depends on make VARIABLES
# (RAM_SIZE, BOOTREQ_NAME) as well as on files, and only regenerating
# unconditionally then comparing catches a variable override.
MEMORY_LD := $(BUILD)/memory.ld

$(MEMORY_LD): FORCE | $(BUILD)
	@addrs=$$(sed -n 's/^#define $(BOOTREQ_NAME)[[:space:]]\{1,\}\(0x[0-9A-Fa-f]\{1,\}\)[uU]\{0,1\}[[:space:]]*\(\/\*.*\)\{0,1\}$$/\1/p' "$(PROTOCOL_H)"); \
	 n=$$(printf '%s\n' "$$addrs" | grep -c . || true); \
	 if [ "$$n" != 1 ]; then \
	   echo "expected exactly one '#define $(BOOTREQ_NAME) 0x...' in $(PROTOCOL_H), found $$n" >&2; \
	   echo "(a second match usually means an old definition was left behind, commented out or inside an #if — sed cannot tell those from the live one, so refuse rather than guess)" >&2; \
	   exit 1; \
	 fi; \
	 addr=$$addrs; \
	 { echo "/* Generated by firmware/Makefile for $(CHIP). DO NOT EDIT. */"; \
	   echo "MEMORY"; \
	   echo "{"; \
	   echo "    FLASH (rx)  : ORIGIN = 0x00000000, LENGTH = 8K"; \
	   echo "    RAM   (xrw) : ORIGIN = 0x20000000, LENGTH = $(RAM_SIZE)"; \
	   echo "}"; \
	   echo "OB_BOOTREQ_ADDR = $$addr;"; \
	 } > "$@.tmp"
	@if cmp -s "$@.tmp" "$@"; then rm -f "$@.tmp"; else mv -f "$@.tmp" "$@"; fi

$(OBJS): Makefile $(CONFIG_H)

$(BUILD)/%.o: %.c | $(BUILD)
	"$(CC)" $(CFLAGS) $(INCLUDES) -MMD -MP -MF "$(@:.o=.d)" -MT "$@" -c "$<" -o "$@"

$(BUILD)/%.o: %.S | $(BUILD)
	"$(AS)" $(CFLAGS) $(INCLUDES) -MMD -MP -MF "$(@:.o=.d)" -MT "$@" -c "$<" -o "$@"

$(BUILD)/$(TARGET).elf: $(OBJS) ports/openboot.ld $(MEMORY_LD) $(ISPLIB_A)
	"$(LD)" $(LDFLAGS) $(OBJS) $(LDLIBS) -o "$@"
	"$(SIZE)" "$@"

# objcopy, then the HARD size gate. The linker MEMORY region already bounds
# the image, but a linker-script regression must not be the only line of
# defense for the one region that can brick the part. .DELETE_ON_ERROR
# removes the oversize .bin so it cannot be flashed by a later target.
$(BUILD)/$(TARGET).bin: $(BUILD)/$(TARGET).elf
	"$(OBJCOPY)" -O binary "$<" "$@"
	@actual=$$(wc -c < "$@"); \
	 if [ "$$actual" -gt $(MAX_IMAGE_BYTES) ]; then \
	   echo "$(TARGET).bin is $$actual bytes; the bootloader region is $(MAX_IMAGE_BYTES) (app starts at 0x2000)" >&2; \
	   exit 1; \
	 fi; \
	 echo "$(TARGET).bin: $$actual / $(MAX_IMAGE_BYTES) bytes"

$(BUILD)/$(TARGET).hex: $(BUILD)/$(TARGET).elf
	"$(OBJCOPY)" -O ihex "$<" "$@"

# Provenance for whoever packages this image. A superproject that vendors
# OpenBoot (composing a factory image, say) otherwise has no way to record
# WHICH bootloader it shipped: the gitlink is not consulted by any build step
# and no revision reaches the artifact.
#
# The image sha256 is the whole statement about configuration — it already
# subsumes every knob, source file and compiler choice — so this deliberately
# does not restate the knobs. It records what produced the bytes and what they
# hash to; openboot_config.h sits beside it for the expanded form.
#
# FORCE + cmp because the contents depend on git state, which moves without
# any file in the build tree changing.
$(BUILD)/$(TARGET).manifest: $(BUILD)/$(TARGET).bin FORCE
	@rev=$$(git -C "$(REPO_ROOT)" rev-parse HEAD 2>/dev/null || echo unknown); \
	 if [ "$$rev" = unknown ]; then dirty=unknown; \
	 elif [ -n "$$(git -C "$(REPO_ROOT)" status --porcelain 2>/dev/null)" ]; then dirty=1; \
	 else dirty=0; fi; \
	 if command -v sha256sum >/dev/null 2>&1; then sum=$$(sha256sum "$<"); \
	 else sum=$$(shasum -a 256 "$<"); fi; \
	 { echo "openboot_revision=$$rev"; \
	   echo "openboot_dirty=$$dirty"; \
	   echo "chip=$(CHIP)"; \
	   echo "transport=$(TRANSPORT)"; \
	   echo "board=$(BOARD)"; \
	   echo "image=$(notdir $<)"; \
	   echo "image_bytes=$$(wc -c < "$<" | tr -d ' ')"; \
	   echo "image_sha256=$${sum%% *}"; \
	 } > "$@.tmp"
	@if cmp -s "$@.tmp" "$@"; then rm -f "$@.tmp"; else mv -f "$@.tmp" "$@"; fi

# SWD flash via a WCH-LinkE. minichlink ignores the return values of its
# CH5xx erase/write calls and its byte-for-byte verify is disabled, so a zero
# exit status is NOT evidence the image landed -- read it back and compare
# before rebooting the target. WCHLINK_SERIAL selects a probe when several
# are attached.
flash: image
	@command -v "$(MINICHLINK)" >/dev/null 2>&1 || { \
	  echo "minichlink not found: $(MINICHLINK)" >&2; exit 1; }
	"$(MINICHLINK)" -C linke \
	    $(if $(strip $(WCHLINK_SERIAL)),-l "$(WCHLINK_SERIAL)") \
	    -w "$(abspath $(BUILD)/$(TARGET).bin)" 0x0
	"$(MINICHLINK)" -C linke \
	    $(if $(strip $(WCHLINK_SERIAL)),-l "$(WCHLINK_SERIAL)") \
	    -r "$(BUILD)/.flash-readback.bin" 0x0 $$(wc -c < "$(BUILD)/$(TARGET).bin")
	cmp "$(BUILD)/.flash-readback.bin" "$(BUILD)/$(TARGET).bin"
	"$(MINICHLINK)" -C linke \
	    $(if $(strip $(WCHLINK_SERIAL)),-l "$(WCHLINK_SERIAL)") -b
	@echo "flashed and read back OK: $(TARGET).bin"

# ROM ISP flash via wchisp (chip in BOOT mode: hold the BOOT strap through a
# power cycle). This is also the recovery path for CH57x-USB images, where
# the USB PHY takes over the SWD pins and kills the debugger.
flash-isp: image
	@command -v "$(WCHISP)" >/dev/null 2>&1 || { \
	  echo "wchisp not found: $(WCHISP)" >&2; exit 1; }
	"$(WCHISP)" flash "$(BUILD)/$(TARGET).bin"

clean:
	rm -rf build

-include $(DEPS)
