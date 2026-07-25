export interface ProcessVersions {
  readonly blitz: string;
}

export interface Process {
  readonly arch: string;
  readonly argv: ReadonlyArray<string>;
  readonly env: { readonly [name: string]: string | undefined };
  readonly execPath: string;
  readonly platform: string;
  readonly version: string;
  readonly versions: ProcessVersions;
  cwd(): string;
}

export declare const arch: string;
export declare const argv: ReadonlyArray<string>;
export declare const cwd: () => string;
export declare const env: { readonly [name: string]: string | undefined };
export declare const execPath: string;
export declare const platform: string;
export declare const version: string;
export declare const versions: ProcessVersions;

declare const process: Process;
export { process as default };
