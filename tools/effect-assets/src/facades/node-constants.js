const values = {
  COPYFILE_EXCL: 1,
  COPYFILE_FICLONE: 2,
  COPYFILE_FICLONE_FORCE: 4,
  F_OK: 0,
  R_OK: 4,
  W_OK: 2,
  X_OK: 1,
  PRIORITY_LOW: 19,
  PRIORITY_BELOW_NORMAL: 10,
  PRIORITY_NORMAL: 0,
  PRIORITY_ABOVE_NORMAL: -7,
  PRIORITY_HIGH: -14,
  PRIORITY_HIGHEST: -20,
  UV_FS_COPYFILE_EXCL: 1,
  UV_FS_COPYFILE_FICLONE: 2,
  UV_FS_COPYFILE_FICLONE_FORCE: 4,
  UV_UDP_REUSEADDR: 4,
};

const constants = Object.freeze(values);

export const COPYFILE_EXCL = constants.COPYFILE_EXCL;
export const COPYFILE_FICLONE = constants.COPYFILE_FICLONE;
export const COPYFILE_FICLONE_FORCE = constants.COPYFILE_FICLONE_FORCE;
export const F_OK = constants.F_OK;
export const R_OK = constants.R_OK;
export const W_OK = constants.W_OK;
export const X_OK = constants.X_OK;
export const PRIORITY_LOW = constants.PRIORITY_LOW;
export const PRIORITY_BELOW_NORMAL = constants.PRIORITY_BELOW_NORMAL;
export const PRIORITY_NORMAL = constants.PRIORITY_NORMAL;
export const PRIORITY_ABOVE_NORMAL = constants.PRIORITY_ABOVE_NORMAL;
export const PRIORITY_HIGH = constants.PRIORITY_HIGH;
export const PRIORITY_HIGHEST = constants.PRIORITY_HIGHEST;
export const UV_FS_COPYFILE_EXCL = constants.UV_FS_COPYFILE_EXCL;
export const UV_FS_COPYFILE_FICLONE = constants.UV_FS_COPYFILE_FICLONE;
export const UV_FS_COPYFILE_FICLONE_FORCE = constants.UV_FS_COPYFILE_FICLONE_FORCE;
export const UV_UDP_REUSEADDR = constants.UV_UDP_REUSEADDR;

export default constants;
