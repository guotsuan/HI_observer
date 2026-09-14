#!/bin/zsh

script_dir="${0:A:h}"
if [[ -x "$script_dir/bin/channelize" ]]; then
    release_root="$script_dir"
else
    release_root="${script_dir:h}"
fi
center_frequency="1420.405751e6"
export SOAPY_SDR_PLUGIN_PATH="$release_root/lib/SoapySDR/modules0.8"

if (( $# > 0 )); then
    center_frequency="$1"
    shift
fi

exec "$release_root/bin/channelize" \
    --freq "$center_frequency" \
    --nch 4096 \
    --lna 5 --mix 5 --vga 5 "$@"
