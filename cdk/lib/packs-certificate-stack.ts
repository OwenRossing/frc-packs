import * as acm from "aws-cdk-lib/aws-certificatemanager";
import * as route53 from "aws-cdk-lib/aws-route53";
import * as cdk from "aws-cdk-lib";
import { Construct } from "constructs";
import { config, fqdn } from "./config";

export interface PacksCertificateStackProps extends cdk.StackProps {}

export class PacksCertificateStack extends cdk.Stack {
  readonly certificate: acm.ICertificate;

  constructor(scope: Construct, id: string, props: PacksCertificateStackProps) {
    super(scope, id, props);

    const hostedZone = config.hostedZoneId
      ? route53.HostedZone.fromHostedZoneAttributes(this, "HostedZone", {
          hostedZoneId: config.hostedZoneId,
          zoneName: config.domain,
        })
      : undefined;
    this.certificate = new acm.Certificate(this, "Certificate", {
      domainName: fqdn,
      validation: hostedZone
        ? acm.CertificateValidation.fromDns(hostedZone)
        : acm.CertificateValidation.fromDns(),
    });

    new cdk.CfnOutput(this, "CertificateArn", { value: this.certificate.certificateArn });
    new cdk.CfnOutput(this, "CertificateDomain", { value: fqdn });
    if (!hostedZone) {
      new cdk.CfnOutput(this, "DnsValidationRequired", {
        value: "Create the ACM DNS validation record manually before using the certificate.",
      });
    }
  }
}
