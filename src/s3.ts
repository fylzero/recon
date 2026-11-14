import { baseName } from "./platform";
import type { S3Location, TransferSource } from "./types";

/** How the backend starts an error for an AWS SSO profile that has to sign in again. */
const SSO_EXPIRED = "Your AWS SSO sign-in has expired.";

export function emptyS3Location(): S3Location {
  return {
    id: "",
    name: "",
    profile: "",
    region: "",
    bucket: "",
    prefix: "",
    endpoint: "",
    encryption: "",
    kmsKeyId: "",
  };
}

export function s3Url(bucket: string, key = "") {
  return `s3://${bucket}/${key}`;
}

export function locationUrl(location: S3Location) {
  return s3Url(location.bucket, location.prefix);
}

/** The last part of a key or folder, without its trailing slash. */
export function keyName(key: string) {
  return key.split("/").filter(Boolean).pop() ?? key;
}

export function sourceName(source: TransferSource) {
  return source.kind === "file" ? baseName(source.path) : keyName(source.key);
}

/** The full path or s3:// URL, for tooltips. */
export function sourcePath(source: TransferSource) {
  return source.kind === "file" ? source.path : s3Url(source.location.bucket, source.key);
}

export function isSsoExpired(err: unknown) {
  return String(err).startsWith(SSO_EXPIRED);
}
