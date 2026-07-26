export interface TimerOptions {
  readonly signal?: AbortSignal;
  /**
   * `ref: false` is rejected by Blitz because the host scheduler cannot
   * unreference a pending timer from runtime liveness.
   */
  readonly ref?: boolean;
}

export function setTimeout<T = void>(
  delay?: number,
  value?: T,
  options?: TimerOptions,
): Promise<T>;
export function setImmediate<T = void>(
  value?: T,
  options?: TimerOptions,
): Promise<T>;
export function setInterval<T = void>(
  delay?: number,
  value?: T,
  options?: TimerOptions,
): AsyncIterableIterator<T>;

export interface Scheduler {
  wait(delay: number, options?: { readonly signal?: AbortSignal }): Promise<void>;
  yield(): Promise<void>;
}

export const scheduler: Scheduler;

declare const timersPromises: {
  readonly scheduler: typeof scheduler;
  readonly setImmediate: typeof setImmediate;
  readonly setInterval: typeof setInterval;
  readonly setTimeout: typeof setTimeout;
};

export { timersPromises as default };
