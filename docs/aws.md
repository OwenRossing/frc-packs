# FRC Packs on AWS

The AWS infrastructure is defined in [`cdk/`](../cdk/). CDK provisions the certificate, API Lambda, DynamoDB table,
static-site bucket, and CloudFront distribution. The site and Lambda assets are built from this repository and deployed
as CDK assets.

```
browser ──► CloudFront
             ├── /api/* ──► API Gateway HTTP API ──► Rust Lambda ──► DynamoDB
             └── everything else ──► private S3 bucket
```

The backend table uses on-demand billing, two all-projection secondary indexes, TTL, point-in-time recovery, and a
retention policy so deleting or replacing the stack does not delete player data. Its keys and item layout are described
in [dynamodb.md](dynamodb.md).

## Requirements

- AWS CLI configured for the account and permissions to deploy CDK stacks.
- Node.js 20 or newer, Rust, and `cargo-lambda`.
- A Route 53 hosted zone for the configured domain. The certificate stack requests its CloudFront certificate in
  `us-east-1` and validates it through Route 53.

The domain, hosted-zone ID, regions, stack names, and `ADMIN_ONLY` default live in [`cdk/lib/config.ts`](../cdk/lib/config.ts).
The frontend and backend deploy to `us-east-2`; the certificate deploys to `us-east-1`.

## First deployment

Install dependencies and bootstrap both deployment regions once:

```sh
npm ci
(cd cdk && npx cdk bootstrap aws://<account-id>/us-east-1 aws://<account-id>/us-east-2)
```

From the repository root, build and deploy all three stacks:

```sh
npm run cdk:deploy
```

The first run builds the web app and ARM64 Lambda package, then deploys the certificate, backend, and frontend stacks.
CDK prints outputs for the API, table, bucket, and distribution. The certificate stack creates a Route 53 DNS
validation record when its hosted zone is configured. Once the distribution is deployed, the frontend stack creates
the configured Route 53 alias record.

Create the initial admin account using the backend stack's `TableName` output:

```sh
cd server
TABLE_NAME=<TableName output> AWS_REGION=us-east-2 cargo run -- create-admin
```

The password is printed once. The default `ADMIN_ONLY` setting is enabled while the deployment is being tested.
Change `adminOnly` in `cdk/lib/config.ts` to `false` and redeploy to allow non-admin accounts to sign in and play.

## Subsequent deployments

After a change to the server, site, photos, pack recipes, or CDK, run this from the repository root:

```sh
npm run cdk:deploy
```

This rebuilds the Lambda and website assets before deploying all stacks. CDK runs the site deployment after the assets,
photos, and packs have uploaded, then invalidates `/*` on CloudFront so the updated site is visible immediately; no
separate GitHub Actions invalidation step is needed.
Mutable root-level app files are cached at CloudFront for up to a day but are not cached by browsers; hashed `/assets/*`
files are cached for a year. Photos are cached for a day, packs for four hours, and `/api/*` is not cached.
Use `npm run cdk:synth` to synthesize the CloudFormation templates locally, `npm run cdk:diff` to review the proposed
changes, and `npm run cdk:test` to run the CDK synthesis checks.

## Notes

- **Free-pack notifications:** the normal server checks once a minute. Lambda only runs while answering requests, so
  scheduled `ready-check` notifications still need an EventBridge schedule; trade notifications work without it.
- **Sign-in limits:** failed guesses are counted in each warm Lambda's memory. AWS WAF rate rules on `/api/login` can
  provide a hard cap.
- **Backups:** point-in-time recovery can restore the DynamoDB table to a point in time in the preceding 35 days.
- **Cost:** the table is pay-per-request and the rest of the stack is largely usage-based. Review AWS pricing before
  relying on estimates.

## Deploying from GitHub

`.github/workflows/deploy.yml` runs server tests against DynamoDB Local and deploys the CDK app after pushes to `main`.
It builds the Lambda package on Linux, avoiding cross-compilation issues on developer machines.

One-time setup:

1. Configure GitHub OIDC in AWS with provider `token.actions.githubusercontent.com` and audience `sts.amazonaws.com`.
2. Create a role trusted by `repo:OwenRossing/frc-packs:ref:refs/heads/main`. Grant it permission to deploy the CDK
   stacks and use their bootstrapped assets in both `us-east-1` and `us-east-2`. Its ARN is configured directly in
   `.github/workflows/deploy.yml`.
