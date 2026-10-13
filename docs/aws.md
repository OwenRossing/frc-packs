# FRC Packs on AWS

```
             CloudFront (packs.owenrossing.me)
              ├── /api/*  ──►  Lambda (the Rust server, built with --features lambda)  ──►  DynamoDB (one table)
              └── everything else  ──►  S3 (the built site, robot photos, pack recipes)
```

Everything is pay-per-use, so a quiet site costs close to nothing: DynamoDB on-demand, Lambda per request, S3 and
CloudFront per byte. The data layout and how cheating is kept out are in [dynamodb.md](dynamodb.md).

The whole setup is one CloudFormation stack, `deploy/aws/template.yaml`, and `deploy/aws/deploy.sh` does each kind of
change in one command.

## What you need

- The AWS CLI, signed in to the account (`aws configure sso`, or `aws sso login` once it's set up).
- Rust with `cargo lambda` (`pip install cargo-lambda`), which builds the server for Lambda's ARM Linux.
- Node 20+ for the site.
- An HTTPS certificate for the domain in **us-east-1** (CloudFront only takes certificates from there).

## First time

1. **Certificate.** In AWS Certificate Manager (region us-east-1), request a public certificate for
   `packs.owenrossing.me` with DNS validation. ACM shows a CNAME record; add it in Cloudflare (DNS only, proxy off).
   When the certificate says Issued, copy its ARN.
2. **A bucket for the server's code.** Any private bucket, e.g. `aws s3 mb s3://frc-packs-code-<something> --region us-east-1`.
3. **Settings.** Make `deploy/aws/.env` (it isn't committed):
   ```sh
   STACK=frc-packs
   REGION=us-east-1
   DOMAIN=packs.owenrossing.me
   CERT_ARN=arn:aws:acm:us-east-1:...:certificate/...
   CODE_BUCKET=frc-packs-code-<something>
   ADMIN_ONLY=1          # only admin accounts can sign in while you test; set 0 to open it up
   ```
4. **Deploy.**
   ```sh
   ./deploy/aws/deploy.sh server   # builds the server and uploads it
   ./deploy/aws/deploy.sh infra    # makes the stack: table, Lambda, bucket, CloudFront (CloudFront takes a few minutes)
   ./deploy/aws/deploy.sh site     # builds the site and uploads it
   ```
   `infra` prints the CloudFront address. In Cloudflare, point `packs` at it with a CNAME, **DNS only (proxy off)**.
5. **The admin account.** Run the server's command against the AWS table from your computer:
   ```sh
   cd server && TABLE_NAME=<the TableName output> AWS_REGION=us-east-1 cargo run -- create-admin
   ```
   It prints the password once.

## Every change after that

| Changed | Run |
| --- | --- |
| The site (`web/`), photos or pack recipes | `./deploy/aws/deploy.sh site` |
| The server (`server/`) | `./deploy/aws/deploy.sh server` |
| Both | `./deploy/aws/deploy.sh all` |
| The template, or `ADMIN_ONLY` | `./deploy/aws/deploy.sh infra` |

Each takes about a minute. Nothing is down while it runs: Lambda switches to the new code between requests, and
CloudFront's cache is cleared after the site upload.

## Things to know

- **Admin-only mode.** With `ADMIN_ONLY=1` nobody but admin accounts can sign in, sign up or play. Flip it to `0` and
  run `deploy.sh infra` to open the site.
- **Free-pack notifications.** On a normal server a loop checks every minute. Lambda only runs while answering
  requests, so that check needs a schedule: an EventBridge rule that runs the function's `ready-check` (not set up
  yet; trade notifications work without it).
- **Sign-in limits** count failed guesses in memory, so each warm Lambda counts separately. It still slows guessing; for
  a hard cap, add AWS WAF rate rules on `/api/login`.
- **Backups.** The table has point-in-time recovery on: it can be restored to any second in the last 35 days.
- **Cost.** At a team's scale: DynamoDB on-demand is cents a month, Lambda and CloudFront stay in or near the free tier,
  and S3 is under a dollar. Check the AWS pricing pages before relying on that.

## Deploying from GitHub

`.github/workflows/deploy.yml` does the same on a Linux machine whenever you push to `main` (or from the Actions tab):
it runs the server tests against a local DynamoDB first, and only then updates AWS. This is also the reliable way to
build the Lambda package: building it on Windows needs Zig, which can be fussy there.

One-time setup:

1. In AWS IAM, add GitHub as an identity provider (`token.actions.githubusercontent.com`, audience `sts.amazonaws.com`).
2. Make a role that `repo:OwenRossing/frc-packs:ref:refs/heads/main` can assume, allowed to update the stack, the
   function, the code bucket and the site bucket, and to clear the CloudFront cache.
3. In the repository's Settings → Secrets and variables → Actions → Variables, set `AWS_ROLE_ARN`, `CODE_BUCKET`
   and (if not us-east-1) `AWS_REGION`. Until `AWS_ROLE_ARN` is set the workflow does nothing.
