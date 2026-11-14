#!/usr/bin/env bash
# Image-only (scanned) PDFs from public sources, so every page needs OCR in both builds.
set -euo pipefail
SRC=~/kbdata/bakeoff/ocr-set/sources; OUT=~/kbdata/ing6/corpus; T=~/kbdata/ing6/tmp
mk() { # name src first last
  local name=$1 src=$2 a=$3 b=$4; rm -rf "$T/$name"; mkdir -p "$T/$name"
  pdftoppm -r 100 -gray -jpeg -jpegopt quality=70 -f "$a" -l "$b" "$SRC/$src" "$T/$name/p"
  uv run -q --with img2pdf python -c "import img2pdf,glob,sys; fs=sorted(glob.glob(sys.argv[1]+'/p-*.jpg')); open(sys.argv[2],'wb').write(img2pdf.convert(fs))" "$T/$name" "$OUT/$name.pdf"
  rm -rf "$T/$name"
}
i=0
for spec in "annual-apple-2024.pdf 1" "annual-apple-2024.pdf 51" "annual-microsoft-2024.pdf 1" "annual-nvidia-2024.pdf 1" "annual-nvidia-2024.pdf 51" "annual-nvidia-2024.pdf 101" \
            "bps-statistik-indonesia-2024.pdf 1" "bps-statistik-indonesia-2024.pdf 101" "bps-statistik-indonesia-2024.pdf 201" "bps-statistik-indonesia-2024.pdf 301" \
            "bps-statistik-telekomunikasi-2024.pdf 1" "bps-statistik-telekomunikasi-2024.pdf 51" "bps-women-and-men-2024.pdf 1" \
            "uu-4-2023.pdf 1" "uu-4-2023.pdf 101" "uu-4-2023.pdf 201" "uu-6-2023.pdf 401" "uu-6-2023.pdf 501" "uu-6-2023.pdf 601" "uu-6-2023.pdf 701"; do
  set -- $spec; i=$((i+1)); mk "scan50-$(printf %02d $i)" "$1" "$2" "$(( $2 + 49 ))"
done
mk big300 uu-6-2023.pdf 1 300
mk small1 perpres-96-2024.pdf 1 1
