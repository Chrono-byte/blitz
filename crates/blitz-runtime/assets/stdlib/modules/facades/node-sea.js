const invalidArgument = (name, expected, value) => {
  const received = value === null
    ? "null"
    : value === undefined
      ? "undefined"
      : typeof value === "object"
        ? "an instance of Object"
        : `type ${typeof value} (${String(value)})`;
  const error = new TypeError(`The \"${name}\" argument must be ${expected}. Received ${received}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  throw error;
};

const outsideSea = () => {
  const error = new Error("Operation cannot be invoked when not in a single-executable application");
  error.code = "ERR_NOT_IN_SINGLE_EXECUTABLE_APPLICATION";
  throw error;
};

export const isSea = () => false;

export const getAsset = (key, encoding) => {
  if (typeof key !== "string") invalidArgument("key", "of type string", key);
  if (encoding !== undefined && typeof encoding !== "string") {
    invalidArgument("encoding", "of type string", encoding);
  }
  return outsideSea();
};

export const getRawAsset = key => {
  if (typeof key !== "string") invalidArgument("key", "of type string", key);
  return outsideSea();
};

export const getAssetAsBlob = key => {
  if (typeof key !== "string") invalidArgument("key", "of type string", key);
  return outsideSea();
};

export const getAssetKeys = () => outsideSea();

const sea = {
  isSea,
  getAsset,
  getRawAsset,
  getAssetAsBlob,
  getAssetKeys,
};

export default sea;
