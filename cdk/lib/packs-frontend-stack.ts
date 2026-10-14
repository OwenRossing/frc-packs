import * as path from "node:path";
import * as acm from "aws-cdk-lib/aws-certificatemanager";
import * as cloudfront from "aws-cdk-lib/aws-cloudfront";
import * as origins from "aws-cdk-lib/aws-cloudfront-origins";
import * as apigatewayv2 from "aws-cdk-lib/aws-apigatewayv2";
import * as route53 from "aws-cdk-lib/aws-route53";
import * as targets from "aws-cdk-lib/aws-route53-targets";
import * as s3 from "aws-cdk-lib/aws-s3";
import * as s3deploy from "aws-cdk-lib/aws-s3-deployment";
import * as cdk from "aws-cdk-lib";
import { Construct } from "constructs";
import { config, fqdn } from "./config";

const BUILD_DIR = path.resolve(__dirname, "../.build");

export interface PacksFrontendStackProps extends cdk.StackProps {
  certificate: acm.ICertificate;
  api: apigatewayv2.HttpApi;
}

export class PacksFrontendStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: PacksFrontendStackProps) {
    super(scope, id, props);

    const siteBucket = new s3.Bucket(this, "SiteBucket", {
      blockPublicAccess: s3.BlockPublicAccess.BLOCK_ALL,
      encryption: s3.BucketEncryption.S3_MANAGED,
      enforceSSL: true,
      objectOwnership: s3.ObjectOwnership.BUCKET_OWNER_ENFORCED,
      removalPolicy: cdk.RemovalPolicy.RETAIN,
    });

    const siteOrigin = origins.S3BucketOrigin.withOriginAccessControl(siteBucket);
    const apiOrigin = new origins.HttpOrigin(
      `${props.api.apiId}.execute-api.${this.region}.${cdk.Aws.URL_SUFFIX}`,
      { protocolPolicy: cloudfront.OriginProtocolPolicy.HTTPS_ONLY },
    );
    const pathRewrite = new cloudfront.Function(this, "StaticPagePaths", {
      code: cloudfront.FunctionCode.fromInline(
        "function handler(event) { var request = event.request; if (request.uri === '/admin') request.uri = '/admin.html'; if (request.uri === '/privacy') request.uri = '/privacy.html'; return request; }",
      ),
    });
    const distribution = new cloudfront.Distribution(this, "Distribution", {
      domainNames: [fqdn],
      certificate: props.certificate,
      defaultRootObject: "index.html",
      defaultBehavior: {
        origin: siteOrigin,
        viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
        cachePolicy: cloudfront.CachePolicy.CACHING_DISABLED,
        functionAssociations: [{
          function: pathRewrite,
          eventType: cloudfront.FunctionEventType.VIEWER_REQUEST,
        }],
      },
      additionalBehaviors: {
        "/api": {
          origin: apiOrigin,
          allowedMethods: cloudfront.AllowedMethods.ALLOW_ALL,
          cachedMethods: cloudfront.CachedMethods.CACHE_GET_HEAD,
          cachePolicy: cloudfront.CachePolicy.CACHING_DISABLED,
          originRequestPolicy: cloudfront.OriginRequestPolicy.ALL_VIEWER_EXCEPT_HOST_HEADER,
          viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
        },
        "/api/*": {
          origin: apiOrigin,
          allowedMethods: cloudfront.AllowedMethods.ALLOW_ALL,
          cachedMethods: cloudfront.CachedMethods.CACHE_GET_HEAD,
          cachePolicy: cloudfront.CachePolicy.CACHING_DISABLED,
          originRequestPolicy: cloudfront.OriginRequestPolicy.ALL_VIEWER_EXCEPT_HOST_HEADER,
          viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
        },
        "/assets/*": {
          origin: siteOrigin,
          viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: cloudfront.CachePolicy.CACHING_OPTIMIZED,
        },
        "/photos/*": {
          origin: siteOrigin,
          viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: cloudfront.CachePolicy.CACHING_OPTIMIZED,
        },
        "/packs/*": {
          origin: siteOrigin,
          viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
          cachePolicy: cloudfront.CachePolicy.CACHING_DISABLED,
        },
      },
    });

    new s3deploy.BucketDeployment(this, "DeploySite", {
      sources: [s3deploy.Source.asset(path.join(BUILD_DIR, "site"))],
      destinationBucket: siteBucket,
      exclude: ["assets/*", "photos/*", "packs/*"],
      cacheControl: [s3deploy.CacheControl.noCache()],
      prune: true,
    });
    new s3deploy.BucketDeployment(this, "DeployAssets", {
      sources: [s3deploy.Source.asset(path.join(BUILD_DIR, "site", "assets"))],
      destinationBucket: siteBucket,
      destinationKeyPrefix: "assets",
      cacheControl: [s3deploy.CacheControl.fromString("public, max-age=31536000, immutable")],
      prune: true,
    });
    new s3deploy.BucketDeployment(this, "DeployPhotos", {
      sources: [s3deploy.Source.asset(path.join(BUILD_DIR, "site", "photos"))],
      destinationBucket: siteBucket,
      destinationKeyPrefix: "photos",
      cacheControl: [s3deploy.CacheControl.fromString("public, max-age=86400")],
      prune: true,
    });
    new s3deploy.BucketDeployment(this, "DeployPacks", {
      sources: [s3deploy.Source.asset(path.join(BUILD_DIR, "site", "packs"))],
      destinationBucket: siteBucket,
      destinationKeyPrefix: "packs",
      cacheControl: [s3deploy.CacheControl.noCache()],
      prune: true,
    });

    if (config.hostedZoneId) {
      const hostedZone = route53.HostedZone.fromHostedZoneAttributes(this, "HostedZone", {
        hostedZoneId: config.hostedZoneId,
        zoneName: config.domain,
      });
      new route53.ARecord(this, "SiteAliasRecord", {
        zone: hostedZone,
        recordName: config.subdomain,
        target: route53.RecordTarget.fromAlias(new targets.CloudFrontTarget(distribution)),
      });
    }

    new cdk.CfnOutput(this, "SiteBucketName", { value: siteBucket.bucketName });
    new cdk.CfnOutput(this, "DistributionDomainName", { value: distribution.distributionDomainName });
    new cdk.CfnOutput(this, "CustomDomain", { value: fqdn });
  }
}
