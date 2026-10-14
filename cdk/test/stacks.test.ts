import * as assert from "node:assert/strict";
import * as cdk from "aws-cdk-lib";
import * as cloudfront from "aws-cdk-lib/aws-cloudfront";
import { Template } from "aws-cdk-lib/assertions";
import { config, fqdn } from "../lib/config";
import { PacksBackendStack } from "../lib/packs-backend-stack";
import { PacksCertificateStack } from "../lib/packs-certificate-stack";
import { PacksFrontendStack } from "../lib/packs-frontend-stack";

const app = new cdk.App();
const certificateStack = new PacksCertificateStack(app, config.stackNames.certificate, {
  env: { account: "111111111111", region: config.regions.certificate },
  crossRegionReferences: true,
});
const backendStack = new PacksBackendStack(app, config.stackNames.backend, {
  env: { account: "111111111111", region: config.regions.application },
});
const frontendStack = new PacksFrontendStack(app, config.stackNames.frontend, {
  env: { account: "111111111111", region: config.regions.application },
  crossRegionReferences: true,
  certificate: certificateStack.certificate,
  api: backendStack.api,
});

assert.equal(certificateStack.stackName, "packs-certificate");
assert.equal(certificateStack.region, "us-east-1");
assert.equal(backendStack.stackName, "packs-backend");
assert.equal(backendStack.region, "us-east-2");
assert.equal(frontendStack.stackName, "packs-frontend");
assert.equal(frontendStack.region, "us-east-2");

const certificateTemplate = Template.fromStack(certificateStack);
certificateTemplate.resourceCountIs("AWS::CertificateManager::Certificate", 1);
certificateTemplate.resourceCountIs("AWS::Route53::RecordSet", 0);
certificateTemplate.hasResourceProperties("AWS::CertificateManager::Certificate", {
  DomainName: fqdn,
  ValidationMethod: "DNS",
});

const backendTemplate = Template.fromStack(backendStack);
backendTemplate.resourceCountIs("AWS::ApiGatewayV2::Api", 1);
backendTemplate.resourceCountIs("AWS::ApiGatewayV2::Route", 2);
backendTemplate.resourceCountIs("AWS::DynamoDB::Table", 1);
backendTemplate.resourceCountIs("AWS::RDS::DBInstance", 0);
backendTemplate.resourceCountIs("AWS::RDS::DBCluster", 0);
backendTemplate.resourceCountIs("AWS::EC2::VPC", 0);
backendTemplate.resourceCountIs("AWS::Events::Rule", 0);
const backendResources = backendTemplate.toJSON().Resources as Record<string, { Type: string; Properties?: Record<string, unknown> }>;
const server = Object.values(backendResources).find((resource) =>
  resource.Type === "AWS::Lambda::Function" && resource.Properties?.Runtime === "provided.al2023");
assert.ok(server);
assert.deepEqual(server.Properties?.Architectures, ["arm64"]);
const environment = (server.Properties?.Environment as { Variables: Record<string, unknown> }).Variables;
assert.equal(environment.COOKIE_SECURE, "1");
assert.equal(environment.DATA_DIR, "/var/task/data");
assert.deepEqual(environment.TABLE_NAME, { Ref: backendTemplate.getResourceId("AWS::DynamoDB::Table") });
assert.equal(environment.ADMIN_ONLY, "0");
assert.equal("DATABASE_URL" in environment, false);
backendTemplate.hasResourceProperties("AWS::DynamoDB::Table", {
  BillingMode: "PAY_PER_REQUEST",
  KeySchema: [
    { AttributeName: "pk", KeyType: "HASH" },
    { AttributeName: "sk", KeyType: "RANGE" },
  ],
  GlobalSecondaryIndexes: [
    {
      IndexName: "gsi1",
      KeySchema: [
        { AttributeName: "gsi1pk", KeyType: "HASH" },
        { AttributeName: "gsi1sk", KeyType: "RANGE" },
      ],
      Projection: { ProjectionType: "ALL" },
    },
    {
      IndexName: "gsi2",
      KeySchema: [
        { AttributeName: "gsi2pk", KeyType: "HASH" },
        { AttributeName: "gsi2sk", KeyType: "RANGE" },
      ],
      Projection: { ProjectionType: "ALL" },
    },
  ],
  TimeToLiveSpecification: { AttributeName: "expires_at", Enabled: true },
  PointInTimeRecoverySpecification: { PointInTimeRecoveryEnabled: true },
});
const tableResource = Object.values(backendTemplate.toJSON().Resources as Record<string, { Type: string; DeletionPolicy?: string; UpdateReplacePolicy?: string }>)
  .find(({ Type }) => Type === "AWS::DynamoDB::Table");
assert.equal(tableResource?.DeletionPolicy, "Retain");
assert.equal(tableResource?.UpdateReplacePolicy, "Retain");
assert.match(JSON.stringify(backendResources), /dynamodb:TransactWriteItems/);
assert.match(JSON.stringify(backendResources), /dynamodb:DescribeTable/);

const frontendTemplate = Template.fromStack(frontendStack);
frontendTemplate.resourceCountIs("AWS::S3::Bucket", 1);
frontendTemplate.resourceCountIs("AWS::CloudFront::Distribution", 1);
frontendTemplate.resourceCountIs("AWS::CloudFront::Function", 1);
frontendTemplate.resourceCountIs("AWS::Route53::RecordSet", config.hostedZoneId ? 1 : 0);
const frontendResources = frontendTemplate.toJSON().Resources as Record<string, { Type: string; Properties?: Record<string, unknown> }>;
const distribution = Object.values(frontendResources).find((resource) => resource.Type === "AWS::CloudFront::Distribution");
assert.ok(distribution);
const distributionConfig = distribution.Properties?.DistributionConfig as {
  Aliases: string[];
  ViewerCertificate: { AcmCertificateArn: unknown };
  DefaultCacheBehavior: {
    FunctionAssociations: Array<{ EventType: string }>;
    CachePolicyId: string;
  };
  CacheBehaviors: Array<{
    PathPattern: string;
    AllowedMethods?: string[];
    CachePolicyId: string;
    OriginRequestPolicyId?: string;
  }>;
};
assert.deepEqual(distributionConfig.Aliases, [fqdn]);
assert.ok(distributionConfig.ViewerCertificate.AcmCertificateArn);
assert.equal(distributionConfig.DefaultCacheBehavior.CachePolicyId, cloudfront.CachePolicy.CACHING_DISABLED.cachePolicyId);
assert.ok(distributionConfig.DefaultCacheBehavior.FunctionAssociations.some(({ EventType }) => EventType === "viewer-request"));
for (const pathPattern of ["/api", "/api/*"]) {
  const behavior = distributionConfig.CacheBehaviors.find((candidate) => candidate.PathPattern === pathPattern);
  assert.ok(behavior);
  assert.ok(behavior.AllowedMethods?.includes("POST"));
  assert.equal(behavior.CachePolicyId, cloudfront.CachePolicy.CACHING_DISABLED.cachePolicyId);
  assert.equal(behavior.OriginRequestPolicyId, cloudfront.OriginRequestPolicy.ALL_VIEWER_EXCEPT_HOST_HEADER.originRequestPolicyId);
}
for (const pathPattern of ["/assets/*", "/photos/*"]) {
  const behavior = distributionConfig.CacheBehaviors.find((candidate) => candidate.PathPattern === pathPattern);
  assert.equal(behavior?.CachePolicyId, cloudfront.CachePolicy.CACHING_OPTIMIZED.cachePolicyId);
}
assert.equal(
  distributionConfig.CacheBehaviors.find(({ PathPattern }) => PathPattern === "/packs/*")?.CachePolicyId,
  cloudfront.CachePolicy.CACHING_DISABLED.cachePolicyId,
);

console.log("CDK stack synthesis checks passed.");
