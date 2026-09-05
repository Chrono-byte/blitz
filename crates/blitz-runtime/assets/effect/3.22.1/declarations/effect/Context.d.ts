import type * as Option from "effect/Option";

interface Pipeable<Self> {
  readonly pipe: {
    <A>(ab: (self: Self) => A): A;
    <A, B>(ab: (self: Self) => A, bc: (value: A) => B): B;
    <A, B, C>(ab: (self: Self) => A, bc: (value: A) => B, cd: (value: B) => C): C;
  };
}

export interface Tag<in out Id, in out Service> extends Pipeable<Tag<Id, Service>> {
  readonly key: string;
  readonly _Service: (_: Service) => Service;
  readonly _Identifier: (_: Id) => Id;
  of(self: Service): Service;
  context(self: Service): Context<Id>;
}

export interface Context<in Services> extends Pipeable<Context<Services>> {
  readonly _Services: (_: Services) => void;
}

export declare const GenericTag: <Id, Service = Id>(key: string) => Tag<Id, Service>;
export declare const isTag: (value: unknown) => value is Tag<unknown, unknown>;
export declare const empty: () => Context<never>;
export declare const make: <Id, Service>(tag: Tag<Id, Service>, service: Service) => Context<Id>;
export declare const add: {
  <Id, Service>(tag: Tag<Id, Service>, service: Service): <Services>(context: Context<Services>) => Context<Services | Id>;
  <Services, Id, Service>(context: Context<Services>, tag: Tag<Id, Service>, service: Service): Context<Services | Id>;
};
export declare const get: {
  <Services, Id extends Services, Service>(tag: Tag<Id, Service>): (context: Context<Services>) => Service;
  <Services, Id extends Services, Service>(context: Context<Services>, tag: Tag<Id, Service>): Service;
};
export declare const unsafeGet: {
  <Id, Service>(tag: Tag<Id, Service>): <Services>(context: Context<Services>) => Service;
  <Services, Id, Service>(context: Context<Services>, tag: Tag<Id, Service>): Service;
};
export declare const getOption: {
  <Id, Service>(tag: Tag<Id, Service>): <Services>(context: Context<Services>) => Option.Option<Service>;
  <Services, Id, Service>(context: Context<Services>, tag: Tag<Id, Service>): Option.Option<Service>;
};
export declare const merge: {
  <Right>(that: Context<Right>): <Left>(self: Context<Left>) => Context<Left | Right>;
  <Left, Right>(self: Context<Left>, that: Context<Right>): Context<Left | Right>;
};
