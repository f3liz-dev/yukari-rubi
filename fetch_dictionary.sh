#!/bin/sh
set -e

DICT_VERSION=${1:-"latest"}
DICT_TYPE=${2:-"core"}

DICT_NAME="sudachi-dictionary-${DICT_VERSION}-${DICT_TYPE}"

echo "Downloading a dictionary file \`${DICT_NAME}\` ..."
echo

curl -L \
    https://d2ej7fkh96fzlu.cloudfront.net/sudachidict/${DICT_NAME}.zip \
    > ${DICT_NAME}.zip

unzip -j ${DICT_NAME}.zip "*/system_${DICT_TYPE}.dic" -d .
rm -f ${DICT_NAME}.zip

# Build dic_converter if not exists
if [ ! -f ./target/release/dic_converter ]; then
    echo "Building dic_converter..."
    cargo build --bin dic_converter --release --manifest-path sudachi-wasm/Cargo.toml
fi

mv "system_${DICT_TYPE}.dic" "dict/system_core.dic"

echo
echo "Placed a compressed dictionary file to \`dict/system_core.dic\` ."
