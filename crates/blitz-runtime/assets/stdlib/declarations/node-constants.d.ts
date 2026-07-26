/** Cross-platform constants whose values are known by Blitz. */
export interface Constants {
  readonly COPYFILE_EXCL: 1;
  readonly COPYFILE_FICLONE: 2;
  readonly COPYFILE_FICLONE_FORCE: 4;
  readonly F_OK: 0;
  readonly R_OK: 4;
  readonly W_OK: 2;
  readonly X_OK: 1;
  readonly PRIORITY_LOW: 19;
  readonly PRIORITY_BELOW_NORMAL: 10;
  readonly PRIORITY_NORMAL: 0;
  readonly PRIORITY_ABOVE_NORMAL: -7;
  readonly PRIORITY_HIGH: -14;
  readonly PRIORITY_HIGHEST: -20;
  readonly UV_FS_COPYFILE_EXCL: 1;
  readonly UV_FS_COPYFILE_FICLONE: 2;
  readonly UV_FS_COPYFILE_FICLONE_FORCE: 4;
  readonly UV_UDP_REUSEADDR: 4;
}

export declare const COPYFILE_EXCL: Constants["COPYFILE_EXCL"];
export declare const COPYFILE_FICLONE: Constants["COPYFILE_FICLONE"];
export declare const COPYFILE_FICLONE_FORCE: Constants["COPYFILE_FICLONE_FORCE"];
export declare const F_OK: Constants["F_OK"];
export declare const R_OK: Constants["R_OK"];
export declare const W_OK: Constants["W_OK"];
export declare const X_OK: Constants["X_OK"];
export declare const PRIORITY_LOW: Constants["PRIORITY_LOW"];
export declare const PRIORITY_BELOW_NORMAL: Constants["PRIORITY_BELOW_NORMAL"];
export declare const PRIORITY_NORMAL: Constants["PRIORITY_NORMAL"];
export declare const PRIORITY_ABOVE_NORMAL: Constants["PRIORITY_ABOVE_NORMAL"];
export declare const PRIORITY_HIGH: Constants["PRIORITY_HIGH"];
export declare const PRIORITY_HIGHEST: Constants["PRIORITY_HIGHEST"];
export declare const UV_FS_COPYFILE_EXCL: Constants["UV_FS_COPYFILE_EXCL"];
export declare const UV_FS_COPYFILE_FICLONE: Constants["UV_FS_COPYFILE_FICLONE"];
export declare const UV_FS_COPYFILE_FICLONE_FORCE: Constants["UV_FS_COPYFILE_FICLONE_FORCE"];
export declare const UV_UDP_REUSEADDR: Constants["UV_UDP_REUSEADDR"];

/** The same immutable object as the named constants. */
declare const constants: Constants;
export { constants as default };
