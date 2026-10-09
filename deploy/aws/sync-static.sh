#!/usr/bin/env bash
# Uploads the built site, the robot photos and the pack recipes to an S3 bucket for CloudFront to serve.
# See docs/aws.md. Needs the AWS CLI signed in with access to the bucket.
#
#   (cd web && npm ci && npm run build)
#   ./deploy/aws/sync-static.sh my-frc-packs-bucket
set -euo pipefail

bucket="${1:?usage: sync-static.sh <bucket>}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
[ -f "$repo/web/dist/index.html" ] || { echo "Build the site first: (cd web && npm ci && npm run build)" >&2; exit 1; }

# Same cache rules the server uses: hashed JS/CSS forever, photos for a day, pages and recipes checked every visit.
aws s3 sync "$repo/web/dist/assets/" "s3://$bucket/assets/" --cache-control "public, max-age=31536000, immutable"
aws s3 sync "$repo/data/photos/" "s3://$bucket/photos/" --cache-control "public, max-age=86400"
aws s3 sync "$repo/data/packs/" "s3://$bucket/packs/" --cache-control "no-cache" --content-type application/json
aws s3 sync "$repo/web/dist/" "s3://$bucket/" --exclude "assets/*" --cache-control "no-cache" --delete \
  --exclude "photos/*" --exclude "packs/*"
# /admin is admin.html (the server routes it; on S3 it needs its own key).
aws s3 cp "$repo/web/dist/admin.html" "s3://$bucket/admin" --content-type text/html --cache-control "no-cache"
echo "Uploaded to s3://$bucket. Invalidate CloudFront if pages look stale: aws cloudfront create-invalidation --distribution-id <id> --paths '/*'"
