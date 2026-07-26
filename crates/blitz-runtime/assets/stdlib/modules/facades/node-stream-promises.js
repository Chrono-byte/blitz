import { promises as streamPromises } from "node:stream";

const { finished, pipeline } = streamPromises;

export { finished, pipeline };
export { streamPromises as default };
