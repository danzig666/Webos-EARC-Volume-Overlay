#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
npm run package
node scripts/release-metadata.mjs
printf '%s\n' 'Release assets ready in build/ and build/release/. Releases are published by the Release workflow when a new version reaches main.'
