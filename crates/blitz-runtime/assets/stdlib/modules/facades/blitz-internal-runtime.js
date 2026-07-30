import { host } from "blitz-internal:host";

const bridge = () => {
  if (host === undefined || host.abiVersion !== 1) {
    throw new Error("Blitz host ABI v1 is not installed");
  }
  const value = host.runtime;
  if (value === undefined) {
    throw new Error("runtime-control primitive is not installed");
  }
  return value;
};

export const reserveRoot = () => {
  const result = bridge().reserveRoot();
  if (result.ok !== true) {
    throw new Error(result.message);
  }
  return result.value;
};
export const attachRoot = (root, interrupt) => bridge().attachRoot(root, interrupt);
export const interruptRoot = (status) => bridge().interruptRoot(status);
export const completeRoot = (root, status) => bridge().completeRoot(root, status);
export const failRoot = (root) => bridge().failRoot(root);
