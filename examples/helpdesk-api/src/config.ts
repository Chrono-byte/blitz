// SPDX-License-Identifier: MPL-2.0

export interface AppConfig {
  readonly hostname: string;
  readonly port: number;
}

const parsePort = (input: string | undefined) => {
  if (input === undefined) {
    return 8000;
  }
  const port = Number(input);
  return Number.isInteger(port) && port >= 0 && port <= 65_535 ? port : 8000;
};

export const config: AppConfig = {
  hostname: process.env.HOST ?? "127.0.0.1",
  port: parsePort(process.env.PORT),
};
