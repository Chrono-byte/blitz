export declare function isSea(): false;
export declare function getAsset(key: string): ArrayBuffer;
export declare function getAsset(key: string, encoding: string): string;
export declare function getRawAsset(key: string): ArrayBuffer;
export declare function getAssetAsBlob(key: string, options?: { type: string }): Blob;
/** Node 24 exposes this in the runtime SEA root as well. */
export declare function getAssetKeys(): string[];

interface SeaModule {
  isSea: typeof isSea;
  getAsset: typeof getAsset;
  getRawAsset: typeof getRawAsset;
  getAssetAsBlob: typeof getAssetAsBlob;
  getAssetKeys: typeof getAssetKeys;
}

declare const sea: SeaModule;
export { sea as default };
