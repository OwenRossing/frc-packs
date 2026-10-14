# AWS deployment infrastructure

The CDK app in `cdk/` defines the infrastructure for `https://packs.7028robotics.com` using three stacks:

- `packs-certificate` in `us-east-1` creates the ACM certificate required by CloudFront.
- `packs-backend` in `us-east-2` creates the HTTP API and Rust Lambda.
- `packs-frontend` in `us-east-2` creates the S3 site bucket, CloudFront distribution, and (when configured) Route 53 alias record.

Stack names, regions, domain, subdomain, and hosted zone ID are in [cdk/config.ts](../cdk/config.ts). Set `HOSTED_ZONE_ID` to the Route 53 hosted zone ID for `7028robotics.com` to enable DNS certificate validation and create the `packs` alias record automatically. Without it, certificate DNS validation is manual and the distribution name is emitted as an output for manual DNS configuration.

```
Browser
  └── CloudFront (packs.7028robotics.com)
       ├── /api/* ──► API Gateway HTTP API ──► Rust Lambda
       └── other paths ──► private S3 bucket (built UI, photos, pack recipes)
```

## Scope and prerequisites

- The stack creates a private, encrypted S3 bucket; CloudFront with origin access control and `/admin` and `/privacy` path rewrites; an API Gateway HTTP API; and an ARM64 Lambda using the `provided.al2023` runtime.
- The frontend, robot photos, and pack recipes are packaged as S3 deployment assets. Cache headers distinguish hashed assets, photos, and revalidated pages/recipes.
- The Lambda package includes the Rust `bootstrap` executable and `data/packs`. It sets `DATA_DIR` and `COOKIE_SECURE`, but intentionally does not configure a database URL, database, VPC, subnets, security groups, or database connectivity. The current server requires Postgres and will not start until the database is configured later.
- Notification infrastructure is excluded. No EventBridge schedule or other replacement notification service is created.
- With `HOSTED_ZONE_ID` configured, the certificate stack uses Route 53 DNS validation and the frontend stack creates the CloudFront alias record. Without it, deploy the certificate stack first, add its ACM validation CNAME manually, and wait for the certificate to become `ISSUED` before deploying the frontend; then point `packs.7028robotics.com` at the `DistributionDomainName` output using the domain's DNS provider.
- The CDK stack itself, API Gateway, Lambda, and S3 bucket are in `us-east-2`. CloudFront is a global service.

## Build and verify (no deployment)

Requirements: Node.js, npm, Rust, `cargo-lambda`, and the repository's normal frontend build dependencies.

```sh
cd cdk
npm ci
cd ../web && npm ci
cd ../cdk
npm run build:artifacts
npm test
npx cdk synth
```

`build:artifacts` builds the Vite frontend and the Rust Lambda binary, then stages site assets and the Lambda package under the ignored `.build/` directory. `npm test` synthesizes the stack and checks key resource counts, API paths, cache behaviors, and excluded database/notification resources. `cdk synth` verifies CloudFormation output. These commands do not deploy or query AWS. Do not run `cdk deploy` as part of this verification.

The CDK deployment creates no database. Once one is available, configure `DATABASE_URL` and the appropriate connectivity separately before expecting the Lambda to start.

## Existing Lambda notes

The same Rust executable supports normal server execution and Lambda via the `lambda` Cargo feature. See [server/Cargo.toml](../server/Cargo.toml) and [server/src/main.rs](../server/src/main.rs). Lambda execution uses the API Gateway HTTP API integration, with `/api` paths, cookies, query strings, and request headers forwarded and API caching disabled.
