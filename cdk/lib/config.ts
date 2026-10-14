export const config = {
  stackNames: {
    frontend: "packs-frontend",
    backend: "packs-backend",
    certificate: "packs-certificate",
  },
  regions: {
    application: "us-east-2",
    certificate: "us-east-1",
  },
  domain: "7028robotics.com",
  subdomain: "packs",
  hostedZoneId: 'Z014656337LGZWAE9JKJP',
  lambda: {
    memorySize: 512,
    timeoutSeconds: 28,
    adminOnly: false,
  },
} as const;

export const fqdn = `${config.subdomain}.${config.domain}`;
