#!/usr/bin/env bash
set -euo pipefail

# Тёмные технические диаграммы из Structurizr DSL:
# DSL --(docker: structurizr/structurizr 6.x)--> PlantUML
#   --(sed: тёмный title + boundary)--> PlantUML SVG --(rsvg-convert)--> PNG (прозрачный фон)
#
# Требования: docker, rsvg-convert (librsvg), ImageMagick (проверки).
# Внимание: structurizr/cli:latest — deprecation-заглушка (баннер, ничего не делает).
# Рабочий образ: structurizr/structurizr (6.x).

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
EXP="${ROOT}/docs/architecture/exports"
IMAGE="structurizr/structurizr:latest"
UID_GID="$(id -u):$(id -g)"

declare -A VIEWS=(
  [landscape/structurizr-SystemContext]=2400
  [platform/structurizr-Containers]=7200
  [platform/structurizr-RentalComponents]=2400
  [operator/structurizr-AppContainers]=2400
)

echo "==> 1/4 Экспорт PlantUML из DSL (structurizr/structurizr:latest)"
for ws in landscape platform operator-app; do
  case "${ws}" in
    operator-app) out_dir="operator" ;;
    *) out_dir="${ws}" ;;
  esac
  docker run --rm --user "${UID_GID}" -v "${ROOT}:/w" "${IMAGE}" \
    export -workspace "/w/docs/architecture/${ws}/workspace.dsl" \
    -format plantuml -output "/w/docs/architecture/exports/${out_dir}" \
    >/dev/null
  echo "    ${ws}: ok"
done

echo "==> 2/4 Перекраска сгенерированного PlantUML (title, boundary, канвас)"
find "${EXP}" -name 'structurizr-*.puml' -exec sed -i \
  -e 's/FontColor: #444444;/FontColor: #8b949e;/g' \
  -e '/\.Boundary-/,/^  }/ s/BackgroundColor: #ffffff;/BackgroundColor: #161b22;/' \
  -e '/  root {/,/^  }/ s/BackgroundColor: #ffffff;/BackgroundColor: transparent;/' \
  {} +

echo "==> 3/4 PlantUML -> SVG"
puml_args=()
for key in "${!VIEWS[@]}"; do puml_args+=("/data/${key}.puml"); done
docker run --rm --user "${UID_GID}" -v "${EXP}:/data" plantuml/plantuml:latest \
  -tsvg -charset UTF-8 "${puml_args[@]}"

echo "==> 4/4 SVG -> PNG (прозрачный фон)"
for key in "${!VIEWS[@]}"; do
  out="${EXP}/$(basename "${key%-*}").png"
  [ "${key}" = "landscape/structurizr-SystemContext" ] && out="${EXP}/landscape.png"
  [ "${key}" = "platform/structurizr-Containers" ] && out="${EXP}/platform-containers.png"
  [ "${key}" = "platform/structurizr-RentalComponents" ] && out="${EXP}/platform-components-rental.png"
  [ "${key}" = "operator/structurizr-AppContainers" ] && out="${EXP}/operator-app.png"
  rsvg-convert -w "${VIEWS[$key]}" "${EXP}/${key}.svg" -o "${out}"
  magick identify -format "    %[fx:w]x%[fx:h] ${out}\n" "${out}"
  # прозрачный угол = фон не запечён
  px=$(magick "${out}" -format "%[pixel:p{1,1}]" info:)
  [[ "${px}" == *"srgba(0,0,0,0)"* || "${px}" == *"rgba(0,0,0,0)"* ]] \
    || { echo "    FAIL: фон не прозрачный (${px}) в ${out}" >&2; exit 1; }
done

echo "OK"
