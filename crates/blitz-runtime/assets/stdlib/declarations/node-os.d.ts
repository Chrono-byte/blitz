/** User data available from the process environment. UID/GID are -1 when unknown. */
export interface UserInfo {
  readonly username: string;
  readonly uid: number;
  readonly gid: number;
  readonly shell: string | null;
  readonly homedir: string;
}

/** The platform-independent constants exposed by Blitz's os slice. */
export interface OsConstants {
  readonly UV_UDP_REUSEADDR: 4;
  readonly priority: {
    readonly PRIORITY_LOW: 19;
    readonly PRIORITY_BELOW_NORMAL: 10;
    readonly PRIORITY_NORMAL: 0;
    readonly PRIORITY_ABOVE_NORMAL: -7;
    readonly PRIORITY_HIGH: -14;
    readonly PRIORITY_HIGHEST: -20;
  };
}

export declare const arch: () => string;
export declare const platform: () => string;
export declare const type: () => string;
export declare const release: () => string;
export declare const hostname: () => string;
/** Throws an error with code ERR_NOT_IMPLEMENTED when no home env is present. */
export declare const homedir: () => string;
export declare const tmpdir: () => string;
/** Throws an error with code ERR_NOT_IMPLEMENTED when user env data is absent. */
export declare const userInfo: () => UserInfo;
export declare const EOL: string;
export declare const devNull: string;
/** Throws an error with code ERR_NOT_IMPLEMENTED for an unknown architecture. */
export declare const endianness: () => "LE" | "BE";
export declare const constants: OsConstants;

/** The process-local initial node:os facade. */
declare const os: {
  readonly arch: typeof arch;
  readonly platform: typeof platform;
  readonly type: typeof type;
  readonly release: typeof release;
  readonly hostname: typeof hostname;
  readonly homedir: typeof homedir;
  readonly tmpdir: typeof tmpdir;
  readonly userInfo: typeof userInfo;
  readonly EOL: typeof EOL;
  readonly devNull: typeof devNull;
  readonly endianness: typeof endianness;
  readonly constants: typeof constants;
};

export { os as default };
