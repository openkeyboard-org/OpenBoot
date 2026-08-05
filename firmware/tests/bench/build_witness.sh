#!/bin/bash
# build_marker.sh <link-base> <marker-addr> <marker-value> <bootreq-addr> <out.bin> <other-addr>
set -euo pipefail
G12="/home/emolitor/Development/Mounriver/Toolchain/RISC-V Embedded GCC12/bin"
SP="$(dirname "$0")"
echo "SECTIONS { . = $1; .text : { *(.text) } }" > "$SP/m.ld"
"$G12/riscv-wch-elf-gcc" -march=rv32imac_zicsr_zifencei -mabi=ilp32 -nostdlib \
  -DMARKER_ADDR=$2 -DMARKER_VALUE=$3 -DBOOTREQ_ADDR=$4 -DOTHER_ADDR=$6 \
  -Wl,-T,"$SP/m.ld" -o "$SP/m.elf" "$SP/marker.S"
"$G12/riscv-wch-elf-objcopy" -O binary "$SP/m.elf" "$5"
printf "  %-14s base=%-8s %s <- %s\n" "$(basename "$5")" "$1" "$2" "$3"
