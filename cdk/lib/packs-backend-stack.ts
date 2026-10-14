import * as path from "node:path";
import * as apigatewayv2 from "aws-cdk-lib/aws-apigatewayv2";
import * as integrations from "aws-cdk-lib/aws-apigatewayv2-integrations";
import * as dynamodb from "aws-cdk-lib/aws-dynamodb";
import * as lambda from "aws-cdk-lib/aws-lambda";
import * as logs from "aws-cdk-lib/aws-logs";
import * as cdk from "aws-cdk-lib";
import { Construct } from "constructs";
import { config } from "./config";

const BUILD_DIR = path.resolve(__dirname, "../.build");

export interface PacksBackendStackProps extends cdk.StackProps {}

export class PacksBackendStack extends cdk.Stack {
  readonly api: apigatewayv2.HttpApi;

  constructor(scope: Construct, id: string, props: PacksBackendStackProps) {
    super(scope, id, props);

    const table = new dynamodb.Table(this, "Table", {
      partitionKey: { name: "pk", type: dynamodb.AttributeType.STRING },
      sortKey: { name: "sk", type: dynamodb.AttributeType.STRING },
      billingMode: dynamodb.BillingMode.PAY_PER_REQUEST,
      pointInTimeRecoverySpecification: { pointInTimeRecoveryEnabled: true },
      timeToLiveAttribute: "expires_at",
      removalPolicy: cdk.RemovalPolicy.RETAIN,
    });
    table.addGlobalSecondaryIndex({
      indexName: "gsi1",
      partitionKey: { name: "gsi1pk", type: dynamodb.AttributeType.STRING },
      sortKey: { name: "gsi1sk", type: dynamodb.AttributeType.STRING },
      projectionType: dynamodb.ProjectionType.ALL,
    });
    table.addGlobalSecondaryIndex({
      indexName: "gsi2",
      partitionKey: { name: "gsi2pk", type: dynamodb.AttributeType.STRING },
      sortKey: { name: "gsi2sk", type: dynamodb.AttributeType.STRING },
      projectionType: dynamodb.ProjectionType.ALL,
    });

    const server = new lambda.Function(this, "ServerFunction", {
      runtime: lambda.Runtime.PROVIDED_AL2023,
      architecture: lambda.Architecture.ARM_64,
      handler: "bootstrap",
      code: lambda.Code.fromAsset(path.join(BUILD_DIR, "lambda")),
      memorySize: config.lambda.memorySize,
      timeout: cdk.Duration.seconds(config.lambda.timeoutSeconds),
      environment: {
        COOKIE_SECURE: "1",
        DATA_DIR: "/var/task/data",
        TABLE_NAME: table.tableName,
        ADMIN_ONLY: config.lambda.adminOnly ? "1" : "0",
      },
      logGroup: new logs.LogGroup(this, "ServerLogs", {
        retention: logs.RetentionDays.ONE_WEEK,
        removalPolicy: cdk.RemovalPolicy.RETAIN,
      }),
    });
    table.grantReadWriteData(server);
    table.grant(server, "dynamodb:DescribeTable", "dynamodb:TransactWriteItems");

    this.api = new apigatewayv2.HttpApi(this, "HttpApi", {
      description: "FRC Packs Rust API",
      createDefaultStage: true,
    });
    const integration = new integrations.HttpLambdaIntegration("ServerIntegration", server);
    this.api.addRoutes({ path: "/api", methods: [apigatewayv2.HttpMethod.ANY], integration });
    this.api.addRoutes({ path: "/api/{proxy+}", methods: [apigatewayv2.HttpMethod.ANY], integration });

    new cdk.CfnOutput(this, "ApiEndpoint", { value: this.api.apiEndpoint });
    new cdk.CfnOutput(this, "TableName", { value: table.tableName });
  }
}
