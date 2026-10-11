import { host } from "blitz-internal:host";

const unavailable = () => {
  const error = new Error("Blitz node:crypto random host primitive is unavailable");
  error.code = "ERR_CRYPTO_HOST_UNAVAILABLE";
  return error;
};

const bridge = () => {
  if (host === undefined || host.abiVersion !== 1) throw unavailable();
  const value = host.nodeCryptoRandom;
  if (value === undefined) throw unavailable();
  return value;
};

export const randomUUID = options => bridge().randomUUID(options);
export const randomBytes = size => bridge().randomBytes(size);
export const randomFill = (buffer, offset, size) => bridge().randomFill(buffer, offset, size);
export const randomInt = (min, max) => bridge().randomInt(min, max);
export const timingSafeEqual = (left, right) => bridge().timingSafeEqual(left, right);
