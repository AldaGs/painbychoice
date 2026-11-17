#!/usr/bin/env bash
# Footage-heavy CPU render benchmark (docs/performance.md, "Rendering footage").
#
# Two 1920x1080 5 Mbps H.264 clips, both on screen, fit into a 1080x1920 comp
# at 24fps with a text layer, rendered by `motion render` to H.264 draft. Then
# the same comp with solids where the clips were, which is the render with the
# decode taken out.
#
#   cargo build --release -p motion-app
#   docs/bench/footage-render.sh [out-dir] [threads...]
#
# Needs ffmpeg on PATH. Clips are generated, so nothing binary is committed.
set -euo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"
motion="${MOTION:-$here/target/release/motion}"
out="${1:-${TMPDIR:-/tmp}/pbc-footage-bench}"
shift || true
threads=("${@:-0}")
mkdir -p "$out"
cd "$out"

for i in 1 2; do
  [ -f clip$i.mp4 ] || ffmpeg -v error -y -f lavfi \
    -i "testsrc2=size=1920x1080:rate=24:duration=10,noise=alls=10:allf=t,hue=h=$((i * 120))" \
    -c:v libx264 -preset veryfast -b:v 5M -maxrate 5M -bufsize 10M -g 48 -pix_fmt yuv420p clip$i.mp4
done

layers() { # $1, $2: what the top and bottom layers draw
  cat <<JSON
    {"name":"subs","type":"text","text":{"string":"AUTO SUBS GO HERE","fontSize":80,"fillColor":[1,1,1],"justification":"center"},"transform":{"position":[540,1700]}},
    {"name":"top",$1,"transform":{"position":[540,560],"scale":[56.25,56.25]}},
    {"name":"bottom",$2,"transform":{"position":[540,1160],"scale":[56.25,56.25]}}
JSON
}
comp() { echo "\"compositions\":[{\"name\":\"main\",\"width\":1080,\"height\":1920,\"duration\":10,\"fps\":24,\"layers\":["; layers "$@"; echo "]}]"; }
{ echo '{"assets":[{"name":"a","path":"clip1.mp4"},{"name":"b","path":"clip2.mp4"}],'; comp '"type":"footage","source":"a"' '"type":"footage","source":"b"'; echo '}'; } > footage.json
{ echo '{'; solid='"type":"solid","color":[0.5,0.2,0.8],"width":1920,"height":1080'; comp "$solid" "$solid"; echo '}'; } > solids.json

for t in "${threads[@]}"; do
  flag=(); [ "$t" != 0 ] && flag=(--threads "$t")
  for spec in footage solids; do
    printf '%-8s threads=%-7s ' "$spec" "${t/#0/default}"
    "$motion" render "$spec.json" --out "$spec-t$t.mp4" "${flag[@]}" 2>&1 | grep -o '[0-9]* frames in .*fps)'
  done
done
