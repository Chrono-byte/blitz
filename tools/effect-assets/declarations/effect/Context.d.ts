export interface Tag<Service, Identifier = Service> {
  readonly key: string;
  readonly _Service: (_: never) => Service;
  readonly _Identifier: (_: Identifier) => void;
}

export interface Context<Services> {
  readonly _Services: (_: Services) => void;
}

export declare const GenericTag: <Service>(key: string) => Tag<Service>;
export declare const empty: () => Context<never>;
export declare const make: <Service, Identifier>(tag: Tag<Service, Identifier>, service: Service) => Context<Identifier>;
export declare const add: <Service, Identifier>(tag: Tag<Service, Identifier>, service: Service) => <Services>(context: Context<Services>) => Context<Services | Identifier>;
export declare const get: <Service, Identifier>(context: Context<Identifier>, tag: Tag<Service, Identifier>) => Service;
