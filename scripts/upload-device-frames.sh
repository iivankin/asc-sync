#!/usr/bin/env bash
set -euo pipefail

source_dir="${ASC_SYNC_DEVICE_FRAMES_SOURCE_DIR:-assets/device-frames}"
remote_prefix="${ASC_SYNC_DEVICE_FRAMES_R2_PREFIX:-assets/device-frames}"
public_url="${ASC_SYNC_DEVICE_FRAMES_URL:-https://orbitstorage.dev/assets/device-frames}"

: "${CLOUDFLARE_R2_ACCESS_KEY_ID:?missing CLOUDFLARE_R2_ACCESS_KEY_ID}"
: "${CLOUDFLARE_R2_SECRET_ACCESS_KEY:?missing CLOUDFLARE_R2_SECRET_ACCESS_KEY}"
: "${CLOUDFLARE_R2_BUCKET:?missing CLOUDFLARE_R2_BUCKET}"
: "${CLOUDFLARE_R2_ENDPOINT:?missing CLOUDFLARE_R2_ENDPOINT}"

if [ ! -d "$source_dir" ]; then
  echo "device frame source directory does not exist: $source_dir" >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
manifest_path="${tmp_dir}/manifest.json"

cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

python3 - "$source_dir" "$manifest_path" "$public_url" <<'PY'
import hashlib
import json
import pathlib
import sys

source_dir = pathlib.Path(sys.argv[1])
manifest_path = pathlib.Path(sys.argv[2])
public_url = sys.argv[3].rstrip("/")

files = []
for path in sorted(source_dir.rglob("*")):
    if not path.is_file():
        continue
    relative_path = path.relative_to(source_dir).as_posix()
    if relative_path == "manifest.json":
        continue
    payload = path.read_bytes()
    files.append({
        "path": relative_path,
        "size": len(payload),
        "md5": hashlib.md5(payload).hexdigest(),
    })

manifest = {
    "version": 1,
    "base_url": public_url,
    "hash": "md5",
    "files": files,
}
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
PY

export AWS_ACCESS_KEY_ID="$CLOUDFLARE_R2_ACCESS_KEY_ID"
export AWS_SECRET_ACCESS_KEY="$CLOUDFLARE_R2_SECRET_ACCESS_KEY"
export AWS_EC2_METADATA_DISABLED=true

destination="s3://${CLOUDFLARE_R2_BUCKET}/${remote_prefix%/}"

if command -v aws >/dev/null 2>&1; then
  aws s3 sync \
    "$source_dir" \
    "$destination" \
    --endpoint-url "$CLOUDFLARE_R2_ENDPOINT" \
    --exclude "manifest.json" \
    --cache-control "public, max-age=3600"

  aws s3 cp \
    "$manifest_path" \
    "${destination}/manifest.json" \
    --endpoint-url "$CLOUDFLARE_R2_ENDPOINT" \
    --content-type application/json \
    --cache-control "no-cache"
elif command -v uv >/dev/null 2>&1; then
  uv run --quiet --with boto3==1.40.73 python3 - "$source_dir" "$manifest_path" "$remote_prefix" <<'PY'
import mimetypes
import os
import pathlib
import sys

import boto3

source_dir = pathlib.Path(sys.argv[1])
manifest_path = pathlib.Path(sys.argv[2])
remote_prefix = sys.argv[3].strip("/")

s3 = boto3.client("s3", endpoint_url=os.environ["CLOUDFLARE_R2_ENDPOINT"])
bucket = os.environ["CLOUDFLARE_R2_BUCKET"]

for path in sorted(source_dir.rglob("*")):
    if not path.is_file():
        continue
    relative_path = path.relative_to(source_dir).as_posix()
    if relative_path == "manifest.json":
        continue
    key = f"{remote_prefix}/{relative_path}"
    content_type = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
    print(f"upload: {relative_path}")
    s3.upload_file(
        str(path),
        bucket,
        key,
        ExtraArgs={
            "CacheControl": "public, max-age=3600",
            "ContentType": content_type,
        },
    )

s3.upload_file(
    str(manifest_path),
    bucket,
    f"{remote_prefix}/manifest.json",
    ExtraArgs={
        "CacheControl": "no-cache",
        "ContentType": "application/json",
    },
)
PY
else
  echo "missing aws CLI; install awscli or uv to upload device frames" >&2
  exit 1
fi

echo "uploaded device frames to ${public_url}"
echo "uploaded hash manifest to ${public_url}/manifest.json"
