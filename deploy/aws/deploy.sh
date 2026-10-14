#!/usr/bin/env bash
# One command per kind of change. Needs the AWS CLI signed in (aws configure sso) to the account that owns the stack.
#
#   ./deploy/aws/deploy.sh infra     create or update the stack (DynamoDB, Lambda, S3, CloudFront) from template.yaml
#   ./deploy/aws/deploy.sh site      build the website and upload it to S3 (then clears CloudFront's cache)
#   ./deploy/aws/deploy.sh server    build the Rust server for Lambda and update the function
#   ./deploy/aws/deploy.sh all       server, site
#
# Settings can sit in deploy/aws/.env (STACK, REGION, DOMAIN, CERT_ARN, CODE_BUCKET, ADMIN_ONLY); it is not committed.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
here="$repo/deploy/aws"
[ -f "$here/.env" ] && . "$here/.env"
STACK="${STACK:-frc-packs}"
REGION="${REGION:-us-east-1}"
DOMAIN="${DOMAIN:-packs.owenrossing.me}"
ADMIN_ONLY="${ADMIN_ONLY:-1}"
CODE_KEY="frc-packs-lambda.zip"
out() { aws cloudformation describe-stacks --stack-name "$STACK" --region "$REGION" --query "Stacks[0].Outputs[?OutputKey=='$1'].OutputValue" --output text; }

build_server() {
  (cd "$repo/server" && cargo lambda build --release --features lambda --arm64)
  local tmp; tmp="$(mktemp -d)"
  cp "$repo/server/target/lambda/frc-packs-server/bootstrap" "$tmp/bootstrap"
  mkdir -p "$tmp/data" && cp -r "$repo/data/packs" "$tmp/data/packs"
  (cd "$tmp" && python -c "import shutil;shutil.make_archive('lambda','zip','.')")
  aws s3 cp "$tmp/lambda.zip" "s3://${CODE_BUCKET:?set CODE_BUCKET}/$CODE_KEY" --region "$REGION"
}

case "${1:-}" in
  infra)
    aws cloudformation deploy --stack-name "$STACK" --region "$REGION" --template-file "$here/template.yaml" --capabilities CAPABILITY_IAM \
      --parameter-overrides DomainName="$DOMAIN" CertificateArn="${CERT_ARN:?set CERT_ARN}" CodeBucket="${CODE_BUCKET:?set CODE_BUCKET}" CodeKey="$CODE_KEY" AdminOnly="$ADMIN_ONLY"
    echo "Point $DOMAIN at: $(out DistributionDomain)" ;;
  site)
    (cd "$repo/web" && npm ci && npm run build)
    "$here/sync-static.sh" "$(out SiteBucketName)"
    aws cloudfront create-invalidation --distribution-id "$(out DistributionId)" --paths '/*' >/dev/null && echo "CloudFront cache cleared." ;;
  server)
    build_server
    aws lambda update-function-code --function-name "$(aws cloudformation describe-stack-resource --stack-name "$STACK" --region "$REGION" --logical-resource-id Fn --query StackResourceDetail.PhysicalResourceId --output text)" \
      --s3-bucket "$CODE_BUCKET" --s3-key "$CODE_KEY" --region "$REGION" >/dev/null && echo "Server updated." ;;
  all) "$0" server; "$0" site ;;
  *) sed -n '2,10p' "$0"; exit 1 ;;
esac
