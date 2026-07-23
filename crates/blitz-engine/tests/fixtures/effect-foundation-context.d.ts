export interface Tag<Id, Service> {
  readonly _Identifier: (_: never) => Id;
  readonly _Service: (_: never) => Service;
}
export declare const GenericTag: <Id, Service>(key: string) => Tag<Id, Service>;
