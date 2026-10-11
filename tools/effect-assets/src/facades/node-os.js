const processValue = globalThis.process;
const processObject = processValue !== null && typeof processValue === "object"
  ? processValue
  : Object.create(null);

const environment = () => {
  const value = processObject.env;
  return value !== null && typeof value === "object" ? value : Object.create(null);
};

const fromEnvironment = (...names) => {
  const env = environment();
  for (const name of names) {
    const value = env[name];
    if (typeof value === "string" && value.length !== 0) return value;
  }
  return undefined;
};

const metadata = (name, fallback) => {
  const value = processObject[name];
  return typeof value === "string" && value.length !== 0 ? value : fallback;
};

const unsupported = operation => {
  const error = new Error(`node:os.${operation} is not implemented by Blitz`);
  error.code = "ERR_NOT_IMPLEMENTED";
  throw error;
};

const platformName = () => metadata("platform", "unknown");
const architecture = () => metadata("arch", "unknown");

const osType = Object.freeze({
  aix: "AIX",
  android: "Android",
  darwin: "Darwin",
  freebsd: "FreeBSD",
  linux: "Linux",
  openbsd: "OpenBSD",
  sunos: "SunOS",
  win32: "Windows_NT",
});

const priority = Object.freeze({
  PRIORITY_LOW: 19,
  PRIORITY_BELOW_NORMAL: 10,
  PRIORITY_NORMAL: 0,
  PRIORITY_ABOVE_NORMAL: -7,
  PRIORITY_HIGH: -14,
  PRIORITY_HIGHEST: -20,
});

export const constants = Object.freeze({
  UV_UDP_REUSEADDR: 4,
  priority,
});

export const arch = architecture;
export const platform = platformName;
export const type = () => {
  const value = osType[platformName()];
  return typeof value === "string" ? value : "Unknown";
};

export const release = () => fromEnvironment("BLITZ_OS_RELEASE") ?? "unknown";

export const hostname = () => fromEnvironment("BLITZ_HOSTNAME", "HOSTNAME", "COMPUTERNAME") ?? "unknown";

export const homedir = () => {
  const value = fromEnvironment("BLITZ_HOME", "HOME", "USERPROFILE");
  return value === undefined ? unsupported("homedir") : value;
};

export const tmpdir = () => {
  const value = platformName() === "win32"
    ? fromEnvironment("BLITZ_TMPDIR", "TEMP", "TMP")
    : fromEnvironment("BLITZ_TMPDIR", "TMPDIR", "TMP", "TEMP");
  if (value !== undefined) return value;

  return platformName() === "win32" ? "C:\\Windows\\Temp" : "/tmp";
};

export const userInfo = () => {
  const username = fromEnvironment("USERNAME", "USER", "LOGNAME");
  const home = fromEnvironment("BLITZ_HOME", "HOME", "USERPROFILE");
  if (username === undefined || home === undefined) return unsupported("userInfo");

  return {
    username,
    uid: -1,
    gid: -1,
    shell: platformName() === "win32" ? null : fromEnvironment("SHELL") ?? null,
    homedir: home,
  };
};

export const EOL = platformName() === "win32" ? "\r\n" : "\n";
export const devNull = platformName() === "win32" ? "NUL" : "/dev/null";

export const endianness = () => {
  const configured = fromEnvironment("BLITZ_ENDIANNESS");
  if (configured === "LE" || configured === "BE") return configured;

  switch (architecture()) {
    case "arm":
    case "arm64":
    case "ia32":
    case "riscv64":
    case "x64":
      return "LE";
    default:
      return unsupported("endianness");
  }
};

const os = Object.freeze({
  arch,
  platform,
  type,
  release,
  hostname,
  homedir,
  tmpdir,
  userInfo,
  EOL,
  devNull,
  endianness,
  constants,
});

export default os;
