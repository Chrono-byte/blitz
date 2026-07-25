// SPDX-License-Identifier: MPL-2.0
var config = { timeout: 5000, retries: 3, debug: false };
var sum = 0;
for (var i = 0; i < 20000; i = i + 1) {
  if (config.debug) sum = sum + config.timeout;
  sum = sum + config.retries;
}
