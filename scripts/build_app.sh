#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build_dir="$repo_root/build"
bundle_dir="$build_dir/macSFTP.app"
iconset_dir="$build_dir/AppIcon.iconset"
# AppIcon.png is the committed raster generated from the editable AppIcon.svg.
# Keep both assets in sync when changing the icon geometry.
icon_source="$repo_root/packaging/macos/AppIcon.png"
plist_template="$repo_root/packaging/macos/Info.plist.in"

package_id="$(cargo pkgid --manifest-path "$repo_root/Cargo.toml" -p macsftp-app)"
version="${package_id##*@}"

cargo build --manifest-path "$repo_root/Cargo.toml" --release -p macsftp-app

rm -rf "$bundle_dir" "$iconset_dir"
mkdir -p "$bundle_dir/Contents/MacOS" "$bundle_dir/Contents/Resources" "$iconset_dir"

cp "$repo_root/target/release/macsftp" "$bundle_dir/Contents/MacOS/macsftp"
chmod 755 "$bundle_dir/Contents/MacOS/macsftp"

while read -r filename pixels; do
    sips -z "$pixels" "$pixels" "$icon_source" --out "$iconset_dir/$filename" >/dev/null
done <<'ICON_SIZES'
icon_16x16.png 16
icon_16x16@2x.png 32
icon_32x32.png 32
icon_32x32@2x.png 64
icon_128x128.png 128
icon_128x128@2x.png 256
icon_256x256.png 256
icon_256x256@2x.png 512
icon_512x512.png 512
icon_512x512@2x.png 1024
ICON_SIZES

if ! iconutil -c icns "$iconset_dir" -o "$bundle_dir/Contents/Resources/AppIcon.icns"; then
    rm -f "$bundle_dir/Contents/Resources/AppIcon.icns"
fi
if [[ ! -s "$bundle_dir/Contents/Resources/AppIcon.icns" ]]; then
    # `iconutil` writes through a private macOS temp directory and can fail in
    # sandboxed builders even when the iconset is valid. ICNS is a small chunk
    # container; these modern PNG chunks provide the same 16–1024px payload.
    icon_output="$bundle_dir/Contents/Resources/AppIcon.icns"
    icon_chunks=(
        "icp4:$iconset_dir/icon_16x16.png"
        "icp5:$iconset_dir/icon_32x32.png"
        "icp6:$iconset_dir/icon_32x32@2x.png"
        "ic07:$iconset_dir/icon_128x128.png"
        "ic08:$iconset_dir/icon_256x256.png"
        "ic09:$iconset_dir/icon_512x512.png"
        "ic10:$iconset_dir/icon_512x512@2x.png"
    )

    write_big_endian_u32() {
        printf '%08x' "$1" | xxd -r -p
    }

    total_size=8
    for chunk in "${icon_chunks[@]}"; do
        file_path="${chunk#*:}"
        file_size="$(stat -f%z "$file_path")"
        total_size=$((total_size + 8 + file_size))
    done

    {
        printf 'icns'
        write_big_endian_u32 "$total_size"
        for chunk in "${icon_chunks[@]}"; do
            chunk_type="${chunk%%:*}"
            file_path="${chunk#*:}"
            file_size="$(stat -f%z "$file_path")"
            printf '%s' "$chunk_type"
            write_big_endian_u32 "$((file_size + 8))"
            command cat "$file_path"
        done
    } > "$icon_output"
fi
sed "s/@VERSION@/$version/g" "$plist_template" > "$bundle_dir/Contents/Info.plist"
printf 'APPL????' > "$bundle_dir/Contents/PkgInfo"

plutil -lint "$bundle_dir/Contents/Info.plist"
local_network_usage_description="$(
    plutil -extract NSLocalNetworkUsageDescription raw "$bundle_dir/Contents/Info.plist"
)"
if [[ -z "$local_network_usage_description" ]]; then
    echo "error: NSLocalNetworkUsageDescription must not be empty" >&2
    exit 1
fi
echo "Built unsigned app bundle: $bundle_dir"
