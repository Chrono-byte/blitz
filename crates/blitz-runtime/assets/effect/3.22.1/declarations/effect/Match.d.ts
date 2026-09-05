export type Pattern = null | undefined | string | number | boolean | Readonly<Record<PropertyKey, unknown>>;

export interface Matcher<Input, Remaining = Input, Output = never> {
  readonly _Input: (_: Input) => void;
  readonly _Remaining: (_: Remaining) => void;
  readonly _Output: (_: never) => Output;
}

export declare const value: <Input>(input: Input) => Matcher<Input>;
export declare const type: <Input>() => Matcher<Input>;
export declare const when: <Matched, Result>(pattern: Pattern, evaluate: (value: Matched) => Result) => <Input, Remaining, Output>(matcher: Matcher<Input, Remaining, Output>) => Matcher<Input, Exclude<Remaining, Matched>, Output | Result>;
export declare const tag: <Tag extends string, Result>(tag: Tag, evaluate: (value: { readonly _tag: Tag }) => Result) => <Input, Remaining, Output>(matcher: Matcher<Input, Remaining, Output>) => Matcher<Input, Exclude<Remaining, { readonly _tag: Tag }>, Output | Result>;
export declare const exhaustive: <Input, Output>(matcher: Matcher<Input, never, Output>) => Output;
export declare const orElse: <Remaining, Result>(fallback: (value: Remaining) => Result) => <Input, Output>(matcher: Matcher<Input, Remaining, Output>) => Output | Result;
export declare const valueTags: <Input extends { readonly _tag: string }, Cases extends Partial<{ readonly [Tag in Input["_tag"]]: (value: Extract<Input, { readonly _tag: Tag }>) => unknown }>>(input: Input, cases: Cases) => ReturnType<Exclude<Cases[keyof Cases], undefined>>;
export declare const typeTags: <Input extends { readonly _tag: string }>() => <Cases extends { readonly [Tag in Input["_tag"]]: (value: Extract<Input, { readonly _tag: Tag }>) => unknown }>(cases: Cases) => (input: Input) => ReturnType<Cases[keyof Cases]>;
